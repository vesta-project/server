//! Bounded browsing snapshots. Installation planning never reads this cache.
use crate::{progress::ProgressReporter, transfer, InstallError, Result};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

pub const CATALOGUE_CACHE_MEMORY_BYTES: usize = 8 * 1024 * 1024;
pub const CATALOGUE_CACHE_DISK_BYTES: usize = 64 * 1024 * 1024;
pub const CATALOGUE_CACHE_ENTRIES: usize = 64;
pub const CATALOGUE_CACHE_DOCUMENT_BYTES: usize = 16 * 1024 * 1024;
const TTL: Duration = Duration::from_secs(300);

/// Protected, optional persistence supplied by the owning host. Implementations
/// must enforce the public disk/entry limits and never accept caller file paths.
/// Calls run on bounded blocking work, never the networking executor.
pub trait CatalogueStore: Send + Sync + 'static {
    fn load(&self, key: &str) -> Option<CatalogueRecord>;
    fn save(&self, records: &[(String, CatalogueRecord)]);
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CatalogueRecord {
    pub bytes: Vec<u8>,
    pub sha256: String,
    pub stored_at_ms: u64,
    pub expires_at_ms: u64,
}
impl CatalogueRecord {
    fn valid(&self, now: u64) -> bool {
        self.bytes.len() <= CATALOGUE_CACHE_DOCUMENT_BYTES
            && self.stored_at_ms <= now
            && now < self.expires_at_ms
            && self.expires_at_ms.saturating_sub(self.stored_at_ms) <= TTL.as_millis() as u64
            && crate::model::sha256(&self.bytes) == self.sha256
    }
}

struct Entry {
    record: Arc<CatalogueRecord>,
    used: u64,
}
#[derive(Default)]
struct Memory {
    entries: HashMap<String, Entry>,
    bytes: usize,
    clock: u64,
}
pub(crate) struct CatalogueCache {
    memory: Mutex<Memory>,
    store: Option<Arc<dyn CatalogueStore>>,
    disk: Arc<Semaphore>,
    queries: tokio::sync::Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    waiters: Arc<Semaphore>,
}
pub(crate) struct QueryGuard {
    _waiter: OwnedSemaphorePermit,
    _key: tokio::sync::OwnedMutexGuard<()>,
}
impl CatalogueCache {
    pub fn new(store: Option<Arc<dyn CatalogueStore>>) -> Arc<Self> {
        Arc::new(Self {
            memory: Mutex::new(Memory::default()),
            store,
            disk: Arc::new(Semaphore::new(1)),
            queries: Default::default(),
            waiters: Arc::new(Semaphore::new(16)),
        })
    }

    pub async fn query(
        &self,
        key: &str,
        job: &transfer::Job,
        reporter: &dyn ProgressReporter,
    ) -> Result<QueryGuard> {
        let waiter = self
            .waiters
            .clone()
            .try_acquire_owned()
            .map_err(|_| InstallError::Busy)?;
        let gate = {
            let mut gates = self.queries.lock().await;
            gates.retain(|_, gate| Arc::strong_count(gate) > 1);
            if let Some(gate) = gates.get(key) {
                gate.clone()
            } else {
                if gates.len() >= 8 {
                    return Err(InstallError::Busy);
                }
                let gate = Arc::new(tokio::sync::Mutex::new(()));
                gates.insert(key.into(), gate.clone());
                gate
            }
        };
        let key = transfer::controlled(
            async { Ok(gate.lock_owned().await) },
            job.deadline,
            reporter,
        )
        .await?;
        Ok(QueryGuard {
            _waiter: waiter,
            _key: key,
        })
    }

    pub async fn get(&self, key: &str) -> Option<Arc<CatalogueRecord>> {
        let now = now_ms();
        if let Ok(mut memory) = self.memory.lock() {
            memory.clock = memory.clock.wrapping_add(1);
            let used = memory.clock;
            if let Some(entry) = memory.entries.get_mut(key) {
                // The digest was checked before insertion, not on every RAM hit.
                if entry.record.stored_at_ms <= now && now < entry.record.expires_at_ms {
                    entry.used = used;
                    return Some(entry.record.clone());
                }
            }
            if let Some(old) = memory.entries.remove(key) {
                memory.bytes -= cost(key, &old.record);
            }
        }
        let store = self.store.clone()?;
        // No disk queue: a busy persistence worker simply causes a provider read.
        let permit = self.disk.clone().try_acquire_owned().ok()?;
        let owned = key.to_owned();
        let record = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            store.load(&owned).filter(|record| record.valid(now_ms()))
        })
        .await
        .ok()??;
        let record = Arc::new(record);
        self.remember(key.to_owned(), record.clone());
        Some(record)
    }

    fn remember(&self, key: String, record: Arc<CatalogueRecord>) {
        let cost = cost(&key, &record);
        if cost > CATALOGUE_CACHE_MEMORY_BYTES {
            return;
        }
        let Ok(mut memory) = self.memory.lock() else {
            return;
        };
        if let Some(old) = memory.entries.remove(&key) {
            memory.bytes -= self::cost(&key, &old.record);
        }
        while memory.bytes + cost > CATALOGUE_CACHE_MEMORY_BYTES
            || memory.entries.len() >= CATALOGUE_CACHE_ENTRIES
        {
            let Some(oldest) = memory
                .entries
                .iter()
                .min_by_key(|(_, entry)| entry.used)
                .map(|(key, _)| key.clone())
            else {
                break;
            };
            if let Some(old) = memory.entries.remove(&oldest) {
                memory.bytes -= self::cost(&oldest, &old.record);
            }
        }
        memory.clock = memory.clock.wrapping_add(1);
        let used = memory.clock;
        memory.bytes += cost;
        memory.entries.insert(key, Entry { record, used });
    }

    /// Only the successfully parsed whole catalogue query commits candidate
    /// source documents. Invalid metadata and cancelled queries leave no entries.
    pub fn commit(&self, documents: Vec<(String, CatalogueRecord)>) {
        for (key, record) in &documents {
            self.remember(key.clone(), Arc::new(record.clone()));
        }
        let Some(store) = self.store.clone() else {
            return;
        };
        let Ok(permit) = self.disk.clone().try_acquire_owned() else {
            return;
        };
        // One owned optional write at a time; cancellation cannot release its
        // permit while synchronous persistence is still running.
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            store.save(&documents);
        });
    }
}

pub(crate) fn record(bytes: Vec<u8>, fresh_for: Duration) -> Option<CatalogueRecord> {
    let ttl = fresh_for.min(TTL);
    if ttl.is_zero() || bytes.len() > CATALOGUE_CACHE_DOCUMENT_BYTES {
        return None;
    }
    let now = now_ms();
    Some(CatalogueRecord {
        sha256: crate::model::sha256(&bytes),
        bytes,
        stored_at_ms: now,
        expires_at_ms: now.saturating_add(ttl.as_millis() as u64),
    })
}
pub(crate) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}
pub(crate) fn remaining(record: &CatalogueRecord) -> Duration {
    Duration::from_millis(record.expires_at_ms.saturating_sub(now_ms()))
}
fn cost(key: &str, record: &CatalogueRecord) -> usize {
    key.len() + record.bytes.len() + record.sha256.len() + std::mem::size_of::<Entry>() + 64
}

/// Conservative freshness: never retain no-store/no-cache or malformed policy.
/// Conditional revalidation is deliberately separate from this first TTL cache.
pub(crate) fn freshness(headers: &reqwest::header::HeaderMap) -> Duration {
    let mut origin_seconds: Option<u64> = None;
    let mut explicit_age = false;
    for value in headers.get_all(reqwest::header::CACHE_CONTROL) {
        let Ok(value) = value.to_str() else {
            return Duration::ZERO;
        };
        for directive in value.split(',') {
            let (name, value) = directive
                .trim()
                .split_once('=')
                .map_or((directive.trim(), None), |(name, value)| {
                    (name.trim(), Some(value.trim()))
                });
            if name.eq_ignore_ascii_case("no-store") || name.eq_ignore_ascii_case("no-cache") {
                return Duration::ZERO;
            }
            if name.eq_ignore_ascii_case("max-age") || name.eq_ignore_ascii_case("s-maxage") {
                explicit_age = true;
                let Some(value) = value else {
                    return Duration::ZERO;
                };
                let Ok(age) = value.trim_matches('"').parse::<u64>() else {
                    return Duration::ZERO;
                };
                origin_seconds = Some(origin_seconds.map_or(age, |previous| previous.min(age)));
            }
        }
    }
    // Until date-based revalidation is implemented, never override an origin's
    // explicit Expires policy with our fallback freshness lifetime.
    if headers.contains_key(reqwest::header::EXPIRES) && !explicit_age {
        return Duration::ZERO;
    }
    let age = match headers.get(reqwest::header::AGE) {
        None => 0,
        Some(value) => match value
            .to_str()
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
        {
            Some(age) => age,
            None => return Duration::ZERO,
        },
    };
    // Subtract origin Age before applying our shorter local freshness cap.
    // Capping max-age first incorrectly expires healthy upstream CDN responses.
    // Without an origin lifetime, this is a five-minute application snapshot
    // from local validation, not a claim about the HTTP representation's age.
    Duration::from_secs(origin_seconds.map_or(300, |seconds| seconds.saturating_sub(age).min(300)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_freshness_is_conservative_and_age_cannot_extend_it() {
        for (control, age, expected) in [
            ("public, max-age=60", "10", 50),
            ("public, max-age=1800", "1499", 300),
            ("public, max-age=1800", "1799", 1),
            ("max-age=9999", "0", 300),
            ("must-revalidate, max-age=10", "11", 0),
            ("no-store", "0", 0),
            ("no-cache", "0", 0),
            ("max-age=bad", "0", 0),
            ("max-age=100", "bad", 0),
        ] {
            let mut headers = reqwest::header::HeaderMap::new();
            headers.insert(reqwest::header::CACHE_CONTROL, control.parse().unwrap());
            headers.insert(reqwest::header::AGE, age.parse().unwrap());
            assert_eq!(freshness(&headers), Duration::from_secs(expected));
        }
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(reqwest::header::AGE, "999999".parse().unwrap());
        assert_eq!(freshness(&headers), Duration::from_secs(300));
        headers.insert(
            reqwest::header::EXPIRES,
            "Wed, 01 Jan 2020 00:00:00 GMT".parse().unwrap(),
        );
        assert!(freshness(&headers).is_zero());
    }

    #[tokio::test]
    async fn memory_is_byte_bounded_and_expired_or_rollback_clock_entries_miss() {
        let cache = CatalogueCache::new(None);
        for index in 0..12 {
            cache.commit(vec![(
                index.to_string(),
                record(vec![0; 1024 * 1024], Duration::from_secs(60)).unwrap(),
            )]);
        }
        assert!(cache.memory.lock().unwrap().bytes <= CATALOGUE_CACHE_MEMORY_BYTES);
        assert!(cache.get("0").await.is_none());
        assert!(cache.get("11").await.is_some());
        let mut expired = record(b"old".to_vec(), Duration::from_secs(1)).unwrap();
        expired.expires_at_ms = now_ms() - 1;
        let mut future = record(b"future".to_vec(), Duration::from_secs(60)).unwrap();
        future.stored_at_ms += 60_000;
        future.expires_at_ms += 60_000;
        cache.commit(vec![("old".into(), expired), ("future".into(), future)]);
        assert!(cache.get("old").await.is_none());
        assert!(cache.get("future").await.is_none());
    }
}
