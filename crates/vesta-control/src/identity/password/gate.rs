use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Instant;

const WINDOW_NANOS: u128 = 60_000_000_000;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Purpose {
    Verification,
    Management,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GateError {
    InvalidConfig,
    RateLimited,
    Busy,
    Closed,
    Unavailable,
}

impl GateError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::InvalidConfig => "invalid_password_gate_config",
            Self::RateLimited => "password_rate_limited",
            Self::Busy => "password_kdf_busy",
            Self::Closed => "password_gate_closed",
            Self::Unavailable => "password_gate_unavailable",
        }
    }
}

impl std::fmt::Display for GateError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("Password work admission failed")
    }
}

impl std::error::Error for GateError {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GateConfig {
    concurrency: usize,
    verification_per_minute: u32,
    management_per_minute: u32,
}

impl GateConfig {
    pub fn new(
        concurrency: usize,
        verification_per_minute: u32,
        management_per_minute: u32,
    ) -> Result<Self, GateError> {
        if !(1..=8).contains(&concurrency)
            || !(1..=10_000).contains(&verification_per_minute)
            || !(1..=1_000).contains(&management_per_minute)
        {
            return Err(GateError::InvalidConfig);
        }
        Ok(Self {
            concurrency,
            verification_per_minute,
            management_per_minute,
        })
    }

    pub const fn concurrency(self) -> usize {
        self.concurrency
    }

    pub const fn verification_per_minute(self) -> u32 {
        self.verification_per_minute
    }

    pub const fn management_per_minute(self) -> u32 {
        self.management_per_minute
    }
}

impl Default for GateConfig {
    fn default() -> Self {
        Self {
            concurrency: 2,
            verification_per_minute: 60,
            management_per_minute: 10,
        }
    }
}

/// Process-local fixed-key admission, not a per-client/fleet quota. Clone shares
/// BOTH rate buckets and the concurrency count. Production composition must share
/// one gate across its password services rather than constructing one per request.
///
/// No derivation waits for a slot and no request creates a bookkeeping key. Only
/// the short accounting critical section uses a mutex; no user callback, database
/// operation or KDF runs under it. Restart resets these nonpersistent budgets.
#[derive(Clone)]
pub struct KdfGate {
    inner: Arc<Inner>,
}

struct Inner {
    config: GateConfig,
    origin: Instant,
    state: Mutex<State>,
    #[cfg(test)]
    clock: Option<Arc<std::sync::atomic::AtomicU64>>,
}

struct State {
    closed: bool,
    active: usize,
    verification: Bucket,
    management: Bucket,
}

struct Bucket {
    rate: u32,
    credit: u128,
    last_nanos: u128,
}

impl Bucket {
    fn new(rate: u32) -> Self {
        Self {
            rate,
            credit: u128::from(rate) * WINDOW_NANOS,
            last_nanos: 0,
        }
    }

    fn consume(&mut self, now: u128) -> bool {
        // Credit has WINDOW_NANOS units per token; refill is exactly rate units
        // per nanosecond. No floating point or discarded fractional refill.
        // A backward clock cannot refill or reset the last observed instant.
        if now > self.last_nanos {
            let elapsed = now - self.last_nanos;
            let refill = elapsed.saturating_mul(u128::from(self.rate));
            let capacity = u128::from(self.rate) * WINDOW_NANOS;
            self.credit = self.credit.saturating_add(refill).min(capacity);
            self.last_nanos = now;
        }
        if self.credit < WINDOW_NANOS {
            return false;
        }
        self.credit -= WINDOW_NANOS;
        true
    }
}

impl Inner {
    fn now(&self) -> u128 {
        #[cfg(test)]
        if let Some(clock) = &self.clock {
            return u128::from(clock.load(std::sync::atomic::Ordering::SeqCst));
        }
        self.origin.elapsed().as_nanos()
    }

    fn lock(&self) -> Result<MutexGuard<'_, State>, GateError> {
        // Unexpected accounting poison refuses all NEW work, rather than trusting
        // potentially incomplete state. Existing permit cleanup still releases
        // its ownership through the recovery path in Drop.
        self.state.lock().map_err(|_| GateError::Unavailable)
    }
}

impl KdfGate {
    pub fn new(config: GateConfig) -> Result<Self, GateError> {
        let config = GateConfig::new(
            config.concurrency,
            config.verification_per_minute,
            config.management_per_minute,
        )?;
        Ok(Self {
            inner: Arc::new(Inner {
                config,
                origin: Instant::now(),
                state: Mutex::new(State {
                    closed: false,
                    active: 0,
                    verification: Bucket::new(config.verification_per_minute),
                    management: Bucket::new(config.management_per_minute),
                }),
                #[cfg(test)]
                clock: None,
            }),
        })
    }

    pub fn try_acquire(&self, purpose: Purpose) -> Result<KdfPermit, GateError> {
        let mut state = self.inner.lock()?;
        if state.closed {
            return Err(GateError::Closed);
        }
        let now = self.inner.now();
        let bucket = match purpose {
            Purpose::Verification => &mut state.verification,
            Purpose::Management => &mut state.management,
        };
        if !bucket.consume(now) {
            return Err(GateError::RateLimited);
        }
        // Saturated attempts consume rate credit too; there is no waiting queue.
        if state.active >= self.inner.config.concurrency {
            return Err(GateError::Busy);
        }
        state.active += 1;
        let permit = KdfPermit {
            inner: Arc::clone(&self.inner),
        };
        drop(state);
        Ok(permit)
    }

    /// Cheap revocation charges management rate WITHOUT requiring a KDF slot.
    pub fn charge_management(&self) -> Result<(), GateError> {
        let mut state = self.inner.lock()?;
        if state.closed {
            return Err(GateError::Closed);
        }
        if !state.management.consume(self.inner.now()) {
            return Err(GateError::RateLimited);
        }
        Ok(())
    }

    /// Reject new admissions. This does not cancel, terminate or wait for active
    /// work, and does not release a permit owned by somebody else. Closing any
    /// clone closes the shared gate; dropping a service must not close a process
    /// budget still in use by another service.
    pub fn close(&self) {
        let mut state = self
            .inner
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.closed = true;
    }

    #[cfg(test)]
    fn with_clock(
        config: GateConfig,
        clock: Arc<std::sync::atomic::AtomicU64>,
    ) -> Result<Self, GateError> {
        let mut gate = Self::new(config)?;
        Arc::get_mut(&mut gate.inner)
            .expect("new gate is uniquely owned")
            .clock = Some(clock);
        Ok(gate)
    }
}

impl std::fmt::Debug for KdfGate {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("KdfGate").finish_non_exhaustive()
    }
}

/// Non-cloneable ownership of exactly one admission. Drop releases on normal
/// return and unwinding. There is no public constructor or reference-counted
/// permit clone that could let work outlive its concurrency ownership.
#[must_use = "dropping the permit releases password work admission"]
pub struct KdfPermit {
    inner: Arc<Inner>,
}

impl Drop for KdfPermit {
    fn drop(&mut self) {
        let mut state = self
            .inner
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        // A private, non-cloneable permit owns one increment. Saturating cleanup
        // avoids a destructor panic if future faulty code poisons/corrupts state;
        // poison still permanently refuses all new admissions.
        state.active = state.active.saturating_sub(1);
    }
}

impl std::fmt::Debug for KdfPermit {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("KdfPermit(<owned>)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Barrier;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Duration;

    fn gate(concurrency: usize, verification: u32, management: u32) -> (KdfGate, Arc<AtomicU64>) {
        let clock = Arc::new(AtomicU64::new(0));
        let config = GateConfig::new(concurrency, verification, management).unwrap();
        let gate = KdfGate::with_clock(config, Arc::clone(&clock)).unwrap();
        (gate, clock)
    }

    fn nanos(duration: Duration) -> u64 {
        u64::try_from(duration.as_nanos()).unwrap()
    }

    #[test]
    fn defaults_and_all_configuration_bounds_are_explicit() {
        assert_eq!(GateConfig::default(), GateConfig::new(2, 60, 10).unwrap());
        for (concurrency, verification, management) in [
            (0, 1, 1),
            (9, 1, 1),
            (1, 0, 1),
            (1, 10_001, 1),
            (1, 1, 0),
            (1, 1, 1_001),
        ] {
            assert_eq!(
                GateConfig::new(concurrency, verification, management).unwrap_err(),
                GateError::InvalidConfig
            );
        }
        assert!(GateConfig::new(8, 10_000, 1_000).is_ok());
    }

    #[test]
    fn exact_refill_boundary_and_fractional_credit_are_preserved() {
        let (gate, clock) = gate(1, 2, 1);
        drop(gate.try_acquire(Purpose::Verification).unwrap());
        drop(gate.try_acquire(Purpose::Verification).unwrap());
        assert_eq!(
            gate.try_acquire(Purpose::Verification).unwrap_err(),
            GateError::RateLimited
        );
        // Repeated fractional advances must not lose refill through rounding.
        for second in 1..30 {
            clock.store(nanos(Duration::from_secs(second)), Ordering::SeqCst);
            assert_eq!(
                gate.try_acquire(Purpose::Verification).unwrap_err(),
                GateError::RateLimited
            );
        }
        clock.store(nanos(Duration::from_secs(30)) - 1, Ordering::SeqCst);
        assert_eq!(
            gate.try_acquire(Purpose::Verification).unwrap_err(),
            GateError::RateLimited
        );
        clock.fetch_add(1, Ordering::SeqCst);
        drop(gate.try_acquire(Purpose::Verification).unwrap());
        assert_eq!(
            gate.try_acquire(Purpose::Verification).unwrap_err(),
            GateError::RateLimited
        );
    }

    #[test]
    fn separate_rates_share_concurrency_and_busy_attempts_are_charged() {
        let (gate, _) = gate(1, 1, 2);
        let held = gate.try_acquire(Purpose::Verification).unwrap();
        let clone = gate.clone();
        assert_eq!(
            clone.try_acquire(Purpose::Management).unwrap_err(),
            GateError::Busy
        );
        assert_eq!(
            clone.try_acquire(Purpose::Verification).unwrap_err(),
            GateError::RateLimited
        );
        drop(held);
        drop(clone.try_acquire(Purpose::Management).unwrap());
        assert_eq!(
            gate.try_acquire(Purpose::Management).unwrap_err(),
            GateError::RateLimited
        );
    }

    #[test]
    fn backwards_time_never_refills_or_resets_observed_time() {
        let (gate, clock) = gate(1, 1, 1);
        drop(gate.try_acquire(Purpose::Verification).unwrap());
        clock.store(nanos(Duration::from_secs(30)), Ordering::SeqCst);
        assert_eq!(
            gate.try_acquire(Purpose::Verification).unwrap_err(),
            GateError::RateLimited
        );
        clock.store(0, Ordering::SeqCst);
        assert_eq!(
            gate.try_acquire(Purpose::Verification).unwrap_err(),
            GateError::RateLimited
        );
        clock.store(nanos(Duration::from_secs(60)) - 1, Ordering::SeqCst);
        assert_eq!(
            gate.try_acquire(Purpose::Verification).unwrap_err(),
            GateError::RateLimited
        );
        clock.fetch_add(1, Ordering::SeqCst);
        drop(gate.try_acquire(Purpose::Verification).unwrap());
    }

    #[test]
    fn idle_refill_is_capped_and_revocation_needs_no_concurrency_slot() {
        let (gate, clock) = gate(1, 1, 1);
        let held = gate.try_acquire(Purpose::Verification).unwrap();
        gate.charge_management().unwrap();
        assert_eq!(
            gate.charge_management().unwrap_err(),
            GateError::RateLimited
        );
        clock.store(
            nanos(Duration::from_secs(10 * 24 * 60 * 60)),
            Ordering::SeqCst,
        );
        gate.charge_management().unwrap();
        assert_eq!(
            gate.charge_management().unwrap_err(),
            GateError::RateLimited
        );
        assert_eq!(
            gate.try_acquire(Purpose::Verification).unwrap_err(),
            GateError::Busy
        );
        drop(held);
        assert_eq!(
            gate.try_acquire(Purpose::Verification).unwrap_err(),
            GateError::RateLimited
        );
    }

    #[test]
    fn concurrent_admission_never_exceeds_the_ceiling_and_permits_reuse_slots() {
        let (gate, _) = gate(2, 100, 100);
        let start = Arc::new(Barrier::new(17));
        let workers: Vec<_> = (0..16)
            .map(|_| {
                let gate = gate.clone();
                let start = Arc::clone(&start);
                std::thread::spawn(move || {
                    start.wait();
                    gate.try_acquire(Purpose::Verification)
                })
            })
            .collect();
        start.wait();
        let mut permits = Vec::new();
        let mut busy = 0;
        for worker in workers {
            match worker.join().unwrap() {
                Ok(permit) => permits.push(permit),
                Err(error) => {
                    assert_eq!(error, GateError::Busy);
                    busy += 1;
                }
            }
        }
        assert_eq!(permits.len(), 2);
        assert_eq!(busy, 14);
        drop(permits);
        let first = gate.try_acquire(Purpose::Management).unwrap();
        let second = gate.try_acquire(Purpose::Management).unwrap();
        assert_eq!(
            gate.try_acquire(Purpose::Management).unwrap_err(),
            GateError::Busy
        );
        drop((first, second));
    }

    #[test]
    fn unwind_releases_ownership_and_close_does_not_release_active_work() {
        let (gate, _) = gate(1, 10, 10);
        let outcome = std::panic::catch_unwind(|| {
            let _held = gate.try_acquire(Purpose::Verification).unwrap();
            panic!("public controlled unwind fixture");
        });
        assert!(outcome.is_err());
        let held = gate.try_acquire(Purpose::Verification).unwrap();
        gate.clone().close();
        gate.close();
        assert_eq!(gate.inner.state.lock().unwrap().active, 1);
        assert_eq!(
            gate.try_acquire(Purpose::Verification).unwrap_err(),
            GateError::Closed
        );
        assert_eq!(gate.charge_management().unwrap_err(), GateError::Closed);
        drop(held);
        assert_eq!(gate.inner.state.lock().unwrap().active, 0);
    }

    #[test]
    fn poisoned_accounting_fails_closed_but_owned_permit_cleanup_remains_safe() {
        let (gate, _) = gate(1, 10, 10);
        let held = gate.try_acquire(Purpose::Verification).unwrap();
        let cloned = gate.clone();
        assert!(
            std::thread::spawn(move || {
                let _accounting = cloned.inner.state.lock().unwrap();
                panic!("public accounting poison fixture");
            })
            .join()
            .is_err()
        );
        assert_eq!(
            gate.try_acquire(Purpose::Verification).unwrap_err(),
            GateError::Unavailable
        );
        assert_eq!(
            gate.charge_management().unwrap_err(),
            GateError::Unavailable
        );
        drop(held);
        assert_eq!(
            gate.inner
                .state
                .lock()
                .err()
                .expect("accounting remains poisoned")
                .into_inner()
                .active,
            0
        );
        gate.close();
    }
}
