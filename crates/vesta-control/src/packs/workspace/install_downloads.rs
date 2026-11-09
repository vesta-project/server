//! Bounded scoped transfers. Every worker is joined before an error escapes.
use super::*;

pub(super) struct AdaptiveDownloads {
    fixed: Option<usize>,
    width: usize,
    previous_rate: Option<f64>,
}
impl AdaptiveDownloads {
    pub fn new(settings: crate::config::DownloadSettings) -> Self {
        Self {
            fixed: settings.max_concurrent.map(usize::from),
            width: 2,
            previous_rate: None,
        }
    }
    pub fn width(&self) -> usize {
        self.fixed.unwrap_or_else(|| {
            self.width.min(
                std::thread::available_parallelism()
                    .map_or(2, |n| n.get().saturating_mul(2))
                    .clamp(2, 8),
            )
        })
    }
    pub fn host_limit(cpu: Option<f64>, used: Option<f64>, total: Option<f64>) -> usize {
        let free = total.zip(used).map(|(total, used)| (total - used).max(0.0));
        if cpu.is_some_and(|cpu| cpu >= 85.0)
            || free.is_some_and(|bytes| bytes < 256.0 * 1024.0 * 1024.0)
        {
            1
        } else if cpu.is_some_and(|cpu| cpu >= 70.0)
            || free.is_some_and(|bytes| bytes < 512.0 * 1024.0 * 1024.0)
        {
            2
        } else {
            8
        }
    }
    pub fn observe(&mut self, bytes: u64, elapsed: Duration) {
        if self.fixed.is_some() || bytes < 1024 * 1024 || elapsed.as_secs_f64() < 0.25 {
            return;
        }
        let rate = bytes as f64 / elapsed.as_secs_f64();
        match self.previous_rate {
            Some(previous) if rate < previous * 0.75 => {
                self.width = self.width.saturating_sub(1).max(1)
            }
            Some(previous) if rate < previous * 0.95 => {}
            _ => self.width = (self.width + 1).min(8),
        }
        self.previous_rate = Some(rate);
    }
}

pub(super) fn batch_map<T: Sync, R: Send>(
    items: &[T],
    fetch: &(impl Fn(&T) -> Result<R> + Sync),
) -> Result<Vec<R>> {
    if items.len() > 16 {
        return Err(super::super::super::PackError::TooLarge.into());
    }
    std::thread::scope(|scope| {
        let mut workers = Vec::new();
        let mut results = Vec::new();
        for (index, item) in items.iter().enumerate() {
            match std::thread::Builder::new()
                .name("vesta-pack-download".into())
                .spawn_scoped(scope, || fetch(item))
            {
                Ok(worker) => workers.push((index, worker)),
                Err(_) => results.push((index, fetch(item))),
            }
        }
        for (index, worker) in workers {
            results.push((
                index,
                worker
                    .join()
                    .unwrap_or_else(|_| Err(super::super::super::PackError::Unavailable.into())),
            ));
        }
        results.sort_by_key(|(index, _)| *index);
        results.into_iter().map(|(_, value)| value).collect()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Condvar, Mutex,
        atomic::{AtomicUsize, Ordering},
    };
    #[test]
    fn automatic_downloads_respond_to_capacity_and_transfer_rates() {
        let mut policy = AdaptiveDownloads::new(Default::default());
        assert_eq!(policy.width(), 2);
        policy.observe(8 * 1024 * 1024, Duration::from_secs(1));
        assert!(policy.width() >= 2);
        policy.observe(1024 * 1024, Duration::from_secs(2));
        assert_eq!(policy.width(), 2);
        assert_eq!(AdaptiveDownloads::host_limit(Some(90.0), None, None), 1);
        assert_eq!(
            AdaptiveDownloads::host_limit(None, Some(900.0e6), Some(1.0e9)),
            1
        );
        assert_eq!(AdaptiveDownloads::host_limit(None, None, None), 8);
        let mut fixed = AdaptiveDownloads::new(crate::config::DownloadSettings {
            max_concurrent: Some(7),
        });
        fixed.observe(8 * 1024 * 1024, Duration::from_secs(1));
        assert_eq!(fixed.width(), 7);
    }
    #[test]
    fn two_downloads_overlap_and_keep_manifest_order() {
        let entered = Mutex::new(0);
        let both = Condvar::new();
        let result = batch_map(&[1, 2], &|value| {
            let mut count = entered.lock().unwrap();
            *count += 1;
            both.notify_all();
            let (count, _) = both
                .wait_timeout_while(count, Duration::from_secs(3), |count| *count < 2)
                .unwrap();
            assert_eq!(*count, 2, "downloads were serialized");
            Ok(value * 10)
        })
        .unwrap();
        assert_eq!(result, [10, 20]);
    }
    #[test]
    fn failures_join_the_other_transfer_and_never_return_partial_results() {
        let finished = AtomicUsize::new(0);
        let result = batch_map(&[1, 2], &|value| {
            finished.fetch_add(1, Ordering::SeqCst);
            if *value == 1 {
                Err(ReleaseError::Conflict)
            } else {
                Ok(20)
            }
        });
        assert!(matches!(result, Err(ReleaseError::Conflict)));
        assert_eq!(finished.load(Ordering::SeqCst), 2);
        assert!(batch_map(&[0; 17], &|_| Ok(())).is_err());
    }
    #[test]
    fn collection_accepts_large_pack_without_retaining_payload_bytes() {
        let mut files = Selected::new();
        let mut used = 0;
        for i in 0..=vesta_protocol::pack::catalogue::MAX_FILES {
            insert_selected(
                &mut files,
                &mut used,
                format!("{i}"),
                Payload {
                    sha256: "a".repeat(64),
                    size: 1024 * 1024,
                },
                None,
            )
            .unwrap();
        }
        assert_eq!(files.len(), vesta_protocol::pack::catalogue::MAX_FILES + 1);
        assert_eq!(
            used,
            (vesta_protocol::pack::catalogue::MAX_FILES as u64 + 1) * 1024 * 1024
        );
        insert_selected(
            &mut files,
            &mut used,
            "0".into(),
            Payload {
                sha256: "b".repeat(64),
                size: 0,
            },
            None,
        )
        .unwrap();
        assert_eq!(
            used,
            vesta_protocol::pack::catalogue::MAX_FILES as u64 * 1024 * 1024
        );
    }
}
