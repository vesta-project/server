use super::provider::{MojangProvider, SessionProvider};
use super::{
    ChallengeId, IssuedChallenge, MinecraftError, MinecraftName, MinecraftUuid, ServerHash,
    ServerUuid, VerifiedMinecraftProfile,
};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, Instant},
};

const TTL: Duration = Duration::from_secs(60);
const MAX_OUTSTANDING: usize = 1024;
const MAX_ACTIVE: usize = 2;
const RATE_BURST: u32 = 60;
const REFILL_INTERVAL: Duration = Duration::from_secs(1);
const NONCE_ATTEMPTS: usize = 4;

pub(super) trait Clock: Send + Sync {
    fn now(&self) -> Instant;
}
struct MonotonicClock;
impl Clock for MonotonicClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
}

struct Limits {
    ttl: Duration,
    max_outstanding: usize,
    max_active: usize,
    issue_burst: u32,
    verify_burst: u32,
    refill_interval: Duration,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            ttl: TTL,
            max_outstanding: MAX_OUTSTANDING,
            max_active: MAX_ACTIVE,
            issue_burst: RATE_BURST,
            verify_burst: RATE_BURST,
            refill_interval: REFILL_INTERVAL,
        }
    }
}

/// Two constant-size buckets, not unbounded per-caller rate accounting.
struct Bucket {
    tokens: u32,
    capacity: u32,
    interval: Duration,
    last_refill: Instant,
}
impl Bucket {
    fn new(capacity: u32, interval: Duration, now: Instant) -> Self {
        Self {
            tokens: capacity,
            capacity,
            interval,
            last_refill: now,
        }
    }
    fn charge(&mut self, now: Instant) -> bool {
        let elapsed = now.saturating_duration_since(self.last_refill);
        let replenished = elapsed.as_nanos() / self.interval.as_nanos();
        if replenished != 0 {
            let remainder = elapsed.as_nanos() % self.interval.as_nanos();
            let remainder = Duration::new(
                (remainder / 1_000_000_000) as u64,
                (remainder % 1_000_000_000) as u32,
            );
            self.last_refill = now.checked_sub(remainder).unwrap_or(now);
            self.tokens = self.capacity.min(
                self.tokens
                    .saturating_add(u32::try_from(replenished).unwrap_or(u32::MAX)),
            );
        }
        if self.tokens == 0 {
            return false;
        }
        self.tokens -= 1;
        true
    }
}

struct Pending {
    server: ServerUuid,
    profile: MinecraftUuid,
    name: MinecraftName,
    hash: ServerHash,
    deadline: Instant,
}
struct State {
    pending: HashMap<ChallengeId, Pending>,
    active: usize,
    issue_rate: Bucket,
    verify_rate: Bucket,
}
struct Shared {
    state: Mutex<State>,
    limits: Limits,
    provider: Arc<dyn SessionProvider>,
    clock: Arc<dyn Clock>,
}
impl Shared {
    fn new(provider: Arc<dyn SessionProvider>, clock: Arc<dyn Clock>, limits: Limits) -> Self {
        let now = clock.now();
        Self {
            state: Mutex::new(State {
                pending: HashMap::new(),
                active: 0,
                issue_rate: Bucket::new(limits.issue_burst, limits.refill_interval, now),
                verify_rate: Bucket::new(limits.verify_burst, limits.refill_interval, now),
            }),
            limits,
            provider,
            clock,
        }
    }
}

static PRODUCTION: OnceLock<Arc<Shared>> = OnceLock::new();

/// Server-bound handles share process-wide pending, verification and rate budgets.
/// Reconstructing or cloning a handle cannot mint a new production budget.
#[derive(Clone)]
pub struct MinecraftChallenges {
    server: ServerUuid,
    shared: Arc<Shared>,
}
impl MinecraftChallenges {
    pub fn new(server: ServerUuid) -> Result<Self, MinecraftError> {
        let shared = PRODUCTION
            .get_or_init(|| {
                Arc::new(Shared::new(
                    Arc::new(MojangProvider::new()),
                    Arc::new(MonotonicClock),
                    Limits::default(),
                ))
            })
            .clone();
        // A poisoned shared state is unavailable, not a fresh independent budget.
        drop(
            shared
                .state
                .lock()
                .map_err(|_| MinecraftError::unavailable())?,
        );
        Ok(Self { server, shared })
    }

    pub fn issue(
        &self,
        profile: MinecraftUuid,
        name: MinecraftName,
    ) -> Result<IssuedChallenge, MinecraftError> {
        let mut state = self
            .shared
            .state
            .lock()
            .map_err(|_| MinecraftError::unavailable())?;
        let now = self.shared.clock.now();
        // Failed capacity admission still spends a token; no cheap busy retry loop.
        if !state.issue_rate.charge(now) {
            return Err(MinecraftError::rate_limited());
        }
        state.pending.retain(|_, item| now < item.deadline);
        if state.pending.len() + state.active >= self.shared.limits.max_outstanding {
            return Err(MinecraftError::busy());
        }
        for _ in 0..NONCE_ATTEMPTS {
            let mut nonce = [0u8; 32];
            getrandom::fill(&mut nonce).map_err(|_| MinecraftError::unavailable())?;
            let id = ChallengeId::from_nonce(&nonce);
            if state.pending.contains_key(&id) {
                continue;
            }
            let hash = server_hash(self.server, &nonce);
            let deadline = self
                .shared
                .clock
                .now()
                .checked_add(self.shared.limits.ttl)
                .ok_or_else(MinecraftError::unavailable)?;
            state.pending.insert(
                id.clone(),
                Pending {
                    server: self.server,
                    profile,
                    name,
                    hash: hash.clone(),
                    deadline,
                },
            );
            return Ok(IssuedChallenge {
                id,
                hash,
                lifetime: self.shared.limits.ttl,
            });
        }
        Err(MinecraftError::unavailable())
    }

    /// Removal precedes binding, expiry, rate/capacity admission and remote I/O.
    /// Every recognized completion attempt consumes its challenge permanently.
    pub fn complete(
        &self,
        challenge: &ChallengeId,
    ) -> Result<Option<VerifiedMinecraftProfile>, MinecraftError> {
        let (pending, _permit) = {
            let mut state = self
                .shared
                .state
                .lock()
                .map_err(|_| MinecraftError::unavailable())?;
            let Some(pending) = state.pending.remove(challenge) else {
                return Ok(None);
            };
            let now = self.shared.clock.now();
            if pending.server != self.server || now >= pending.deadline {
                return Ok(None);
            }
            if !state.verify_rate.charge(now) {
                return Err(MinecraftError::rate_limited());
            }
            if state.active >= self.shared.limits.max_active {
                return Err(MinecraftError::busy());
            }
            state.active += 1;
            let permit = VerificationPermit(self.shared.clone());
            (pending, permit)
        };
        // Neither DNS nor HTTP holds the state lock. The permit survives until the
        // actual provider returns, including DNS timeout overruns and unwinding.
        let joined = self.shared.provider.verify(&pending.name, &pending.hash)?;
        if self.shared.clock.now() >= pending.deadline {
            return Ok(None);
        }
        let Some(joined) = joined else {
            return Ok(None);
        };
        if joined.uuid != pending.profile
            || !pending
                .name
                .as_str()
                .eq_ignore_ascii_case(joined.name.as_str())
        {
            return Ok(None);
        }
        Ok(Some(VerifiedMinecraftProfile {
            server: pending.server,
            uuid: joined.uuid,
            name: joined.name,
        }))
    }

    #[cfg(test)]
    pub(super) fn compose(
        server: ServerUuid,
        provider: Arc<dyn SessionProvider>,
        clock: Arc<dyn Clock>,
        limits: TestLimits,
    ) -> Result<Self, MinecraftError> {
        let limits = limits.validated()?;
        Ok(Self {
            server,
            shared: Arc::new(Shared::new(provider, clock, limits)),
        })
    }

    #[cfg(test)]
    pub(super) fn handle_for_server(&self, server: ServerUuid) -> Self {
        Self {
            server,
            shared: self.shared.clone(),
        }
    }
}

struct VerificationPermit(Arc<Shared>);
impl Drop for VerificationPermit {
    fn drop(&mut self) {
        let mut state = self
            .0
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        state.active -= 1;
    }
}

/// Mojang requires an unpredictable shared serverId of at most 40 hex characters,
/// not SHA-1 itself. This new construction intentionally has no Java digest fixture.
fn server_hash(server: ServerUuid, nonce: &[u8; 32]) -> ServerHash {
    let mut digest = Sha256::new();
    digest.update(b"vesta-minecraft-session-v1\0");
    digest.update(server.as_uuid().as_bytes());
    digest.update(nonce);
    let digest = digest.finalize();
    let mut shortened = [0u8; 20];
    shortened.copy_from_slice(&digest[..20]);
    ServerHash::from_digest(&shortened)
}

#[cfg(test)]
pub(super) struct TestLimits {
    pub(super) ttl: Duration,
    pub(super) max_outstanding: usize,
    pub(super) max_active: usize,
    pub(super) issue_burst: u32,
    pub(super) verify_burst: u32,
    pub(super) refill_interval: Duration,
}
#[cfg(test)]
impl Default for TestLimits {
    fn default() -> Self {
        Self {
            ttl: TTL,
            max_outstanding: MAX_OUTSTANDING,
            max_active: MAX_ACTIVE,
            issue_burst: RATE_BURST,
            verify_burst: RATE_BURST,
            refill_interval: REFILL_INTERVAL,
        }
    }
}
#[cfg(test)]
impl TestLimits {
    fn validated(self) -> Result<Limits, MinecraftError> {
        if !(Duration::from_secs(1)..=Duration::from_secs(120)).contains(&self.ttl)
            || !(1..=MAX_OUTSTANDING).contains(&self.max_outstanding)
            || !(1..=MAX_ACTIVE).contains(&self.max_active)
            || self.max_active > self.max_outstanding
            || !(1..=RATE_BURST).contains(&self.issue_burst)
            || !(1..=RATE_BURST).contains(&self.verify_burst)
            || !(REFILL_INTERVAL..=Duration::from_secs(120)).contains(&self.refill_interval)
        {
            return Err(MinecraftError::invalid());
        }
        Ok(Limits {
            ttl: self.ttl,
            max_outstanding: self.max_outstanding,
            max_active: self.max_active,
            issue_burst: self.issue_burst,
            verify_burst: self.verify_burst,
            refill_interval: self.refill_interval,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn production_factories_and_clones_share_all_state_without_provider_io() {
        let server = ServerUuid::parse("aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee").unwrap();
        let other = ServerUuid::parse("bbbbbbbb-cccc-4ddd-8eee-ffffffffffff").unwrap();
        let first = MinecraftChallenges::new(server).unwrap();
        let reconstructed = MinecraftChallenges::new(server).unwrap();
        let other_server = MinecraftChallenges::new(other).unwrap();
        let cloned = first.clone();
        for handle in [&reconstructed, &other_server, &cloned] {
            // State, both buckets, provider and clock are all owned by this one Arc.
            assert!(Arc::ptr_eq(&first.shared, &handle.shared));
        }
        assert_eq!(first.server, server);
        assert_eq!(other_server.server, other);
        assert_eq!(first.shared.limits.ttl, Duration::from_secs(60));
        assert_eq!(first.shared.limits.max_outstanding, 1024);
        assert_eq!(first.shared.limits.max_active, 2);
        assert_eq!(first.shared.limits.issue_burst, 60);
        assert_eq!(first.shared.limits.verify_burst, 60);
        assert_eq!(first.shared.limits.refill_interval, Duration::from_secs(1));
        // Never call complete on a production handle: even a faulty regression must
        // not accidentally send a real hasJoined request.
    }

    #[test]
    fn rate_refill_preserves_fractional_time_and_never_refills_on_backward_samples() {
        let start = Instant::now();
        let mut bucket = Bucket::new(1, Duration::from_secs(1), start);
        assert!(bucket.charge(start));
        assert!(!bucket.charge(start + Duration::from_millis(999)));
        assert!(bucket.charge(start + Duration::from_millis(1500)));
        assert!(!bucket.charge(start + Duration::from_millis(1999)));
        assert!(bucket.charge(start + Duration::from_secs(2)));
        assert!(!bucket.charge(start + Duration::from_secs(1)));
        assert!(!bucket.charge(start + Duration::from_millis(2999)));
        assert!(bucket.charge(start + Duration::from_secs(3)));
    }

    #[test]
    fn idle_refill_is_capped_at_the_burst_not_an_accumulated_backlog() {
        let start = Instant::now();
        let mut bucket = Bucket::new(2, Duration::from_secs(1), start);
        let later = start + Duration::from_secs(3600);
        assert!(bucket.charge(later));
        assert!(bucket.charge(later));
        assert!(!bucket.charge(later));
    }
}
