//! Bounded monotonic peer admission, not authentication or management authority.
//!
//! The host owns one limiter for its lifetime and supplies the observed socket IP,
//! never a caller-selected header or identity claim. Permits grant no pack/session
//! or administrative capability. There is no listener, worker or global singleton.
use std::{
    collections::HashMap,
    fmt,
    net::IpAddr,
    time::{Duration, Instant},
};

pub const MAX_TRACKED_PEERS: usize = 4_096;
pub const MAX_BURST: u32 = 1_000_000;
pub const MIN_WINDOW: Duration = Duration::from_millis(1);
pub const MAX_WINDOW: Duration = Duration::from_secs(3_600);
pub const MAX_IDLE: Duration = Duration::from_secs(86_400);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvalidRatePolicy;
impl fmt::Display for InvalidRatePolicy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Rate policy invalid")
    }
}
impl std::error::Error for InvalidRatePolicy {}

/// Validated limits. Idle eviction additionally requires a completely refilled
/// bucket, so a short retention period cannot reset a still-depleted budget.
#[derive(Clone, Copy, Debug)]
pub struct RatePolicy {
    burst: u32,
    window: Duration,
    peers: usize,
    idle: Duration,
}
impl RatePolicy {
    pub fn new(
        burst: u32,
        window: Duration,
        peers: usize,
        idle: Duration,
    ) -> Result<Self, InvalidRatePolicy> {
        if burst == 0
            || burst > MAX_BURST
            || !(MIN_WINDOW..=MAX_WINDOW).contains(&window)
            || peers == 0
            || peers > MAX_TRACKED_PEERS
            || idle.is_zero()
            || idle > MAX_IDLE
        {
            return Err(InvalidRatePolicy);
        }
        Ok(Self {
            burst,
            window,
            peers,
            idle,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AdmissionDecision {
    Allowed,
    RateLimited {
        retry_after: Duration,
    },
    /// All tracked slots are occupied. No active peer is evicted to admit a new one.
    Saturated,
    ClockUnavailable,
}

struct Bucket {
    credit: u128,
    updated: Instant,
    seen: Instant,
}

/// Single-owner bounded map. Synchronization, if needed, belongs to the host;
/// calling this component does not create detached cleanup tasks.
pub struct PeerRateLimiter {
    policy: RatePolicy,
    buckets: HashMap<IpAddr, Bucket>,
    observed: Option<Instant>,
}
impl fmt::Debug for PeerRateLimiter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PeerRateLimiter")
            .field("tracked_peers", &self.buckets.len())
            .finish_non_exhaustive()
    }
}
impl PeerRateLimiter {
    pub fn new(policy: RatePolicy) -> Self {
        Self {
            policy,
            buckets: HashMap::new(),
            observed: None,
        }
    }

    /// Consume exactly one permit for the backend-observed peer address.
    pub fn try_acquire(&mut self, peer: IpAddr) -> AdmissionDecision {
        self.acquire_at(peer, Instant::now())
    }

    pub fn tracked_peers(&self) -> usize {
        self.buckets.len()
    }

    /// Bounded, synchronous cleanup; never removes a partially depleted bucket.
    pub fn prune_idle(&mut self) -> usize {
        let now = Instant::now();
        if !self.observe(now) {
            return 0;
        }
        self.prune_at(now)
    }

    fn observe(&mut self, now: Instant) -> bool {
        if self.observed.is_some_and(|last| now < last) {
            return false;
        }
        self.observed = Some(now);
        true
    }

    fn acquire_at(&mut self, peer: IpAddr, now: Instant) -> AdmissionDecision {
        if !self.observe(now) {
            return AdmissionDecision::ClockUnavailable;
        }
        // A dual-stack socket must not grant a second budget for the same IPv4 peer.
        let peer = match peer {
            IpAddr::V6(address) => address.to_ipv4_mapped().map(IpAddr::V4).unwrap_or(peer),
            _ => peer,
        };
        let token = self.policy.window.as_nanos();
        let capacity = token * u128::from(self.policy.burst);
        if !self.buckets.contains_key(&peer) && self.buckets.len() == self.policy.peers {
            self.prune_at(now);
            if self.buckets.len() == self.policy.peers {
                return AdmissionDecision::Saturated;
            }
        }
        let bucket = self.buckets.entry(peer).or_insert_with(|| Bucket {
            credit: capacity,
            updated: now,
            seen: now,
        });
        refill(bucket, now, self.policy.burst, capacity);
        // Rejected attempts count as activity; hammering a depleted bucket cannot
        // cause idle cleanup to reset it while requests are still arriving.
        bucket.seen = now;
        if bucket.credit >= token {
            bucket.credit -= token;
            AdmissionDecision::Allowed
        } else {
            let deficit = token - bucket.credit;
            let nanos = deficit.div_ceil(u128::from(self.policy.burst));
            // Policy bounds guarantee one-token wait <= the one-hour window.
            AdmissionDecision::RateLimited {
                retry_after: Duration::from_nanos(nanos as u64),
            }
        }
    }

    fn prune_at(&mut self, now: Instant) -> usize {
        let before = self.buckets.len();
        let capacity = self.policy.window.as_nanos() * u128::from(self.policy.burst);
        self.buckets.retain(|_, bucket| {
            refill(bucket, now, self.policy.burst, capacity);
            now.duration_since(bucket.seen) < self.policy.idle || bucket.credit < capacity
        });
        before - self.buckets.len()
    }
}

fn refill(bucket: &mut Bucket, now: Instant, burst: u32, capacity: u128) {
    let elapsed = now.duration_since(bucket.updated).as_nanos();
    bucket.credit = bucket
        .credit
        .saturating_add(elapsed.saturating_mul(u128::from(burst)))
        .min(capacity);
    bucket.updated = now;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, Ipv6Addr};

    const A: IpAddr = IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1));
    const B: IpAddr = IpAddr::V4(Ipv4Addr::new(192, 0, 2, 2));
    fn limiter(burst: u32, seconds: u64, peers: usize, idle: u64) -> PeerRateLimiter {
        PeerRateLimiter::new(
            RatePolicy::new(
                burst,
                Duration::from_secs(seconds),
                peers,
                Duration::from_secs(idle),
            )
            .unwrap(),
        )
    }

    #[test]
    fn burst_and_exact_continuous_refill_preserve_fractional_credit() {
        let now = Instant::now();
        let mut limiter = limiter(3, 1, 2, 1);
        for _ in 0..3 {
            assert_eq!(limiter.acquire_at(A, now), AdmissionDecision::Allowed);
        }
        assert_eq!(
            limiter.acquire_at(A, now),
            AdmissionDecision::RateLimited {
                retry_after: Duration::from_nanos(333_333_334)
            }
        );
        assert_eq!(
            limiter.acquire_at(A, now + Duration::from_nanos(333_333_333)),
            AdmissionDecision::RateLimited {
                retry_after: Duration::from_nanos(1)
            }
        );
        assert_eq!(
            limiter.acquire_at(A, now + Duration::from_nanos(333_333_334)),
            AdmissionDecision::Allowed
        );
        assert_eq!(
            limiter.acquire_at(A, now + Duration::from_nanos(666_666_667)),
            AdmissionDecision::Allowed
        );
        assert_eq!(
            limiter.acquire_at(A, now + Duration::from_secs(1)),
            AdmissionDecision::Allowed
        );
    }

    #[test]
    fn retry_matches_the_window_and_addresses_have_independent_budgets() {
        let now = Instant::now();
        let mut limiter = limiter(1, 60, 2, 1);
        assert_eq!(limiter.acquire_at(A, now), AdmissionDecision::Allowed);
        assert_eq!(
            limiter.acquire_at(A, now),
            AdmissionDecision::RateLimited {
                retry_after: Duration::from_secs(60)
            }
        );
        assert_eq!(limiter.acquire_at(B, now), AdmissionDecision::Allowed);
        assert_eq!(
            limiter.acquire_at(A, now + Duration::from_secs(60)),
            AdmissionDecision::Allowed
        );
        assert_eq!(
            limiter.acquire_at(A, now + Duration::from_secs(60)),
            AdmissionDecision::RateLimited {
                retry_after: Duration::from_secs(60)
            }
        );
    }

    #[test]
    fn saturation_never_evicts_active_or_depleted_peers() {
        let now = Instant::now();
        let mut limiter = limiter(1, 60, 1, 1);
        assert_eq!(limiter.acquire_at(A, now), AdmissionDecision::Allowed);
        assert_eq!(
            limiter.acquire_at(B, now + Duration::from_secs(2)),
            AdmissionDecision::Saturated
        );
        assert_eq!(limiter.tracked_peers(), 1);
        assert_eq!(
            limiter.acquire_at(A, now + Duration::from_secs(2)),
            AdmissionDecision::RateLimited {
                retry_after: Duration::from_secs(58)
            }
        );
        assert_eq!(
            limiter.acquire_at(B, now + Duration::from_secs(60)),
            AdmissionDecision::Allowed
        );
        assert_eq!(limiter.tracked_peers(), 1);
    }

    #[test]
    fn rejected_activity_retains_a_slot_even_after_full_refill() {
        let now = Instant::now();
        let mut limiter = limiter(1, 60, 1, 10);
        assert_eq!(limiter.acquire_at(A, now), AdmissionDecision::Allowed);
        assert_eq!(
            limiter.acquire_at(A, now + Duration::from_secs(59)),
            AdmissionDecision::RateLimited {
                retry_after: Duration::from_secs(1)
            }
        );
        assert_eq!(
            limiter.acquire_at(B, now + Duration::from_secs(60)),
            AdmissionDecision::Saturated
        );
        assert_eq!(
            limiter.acquire_at(B, now + Duration::from_secs(69)),
            AdmissionDecision::Allowed
        );
    }

    #[test]
    fn hard_map_bound_refuses_an_additional_unique_peer() {
        let now = Instant::now();
        let mut limiter = limiter(1, 60, MAX_TRACKED_PEERS, 10);
        for index in 0..MAX_TRACKED_PEERS {
            let peer = IpAddr::V4(Ipv4Addr::from(0xc000_0000 + index as u32));
            assert_eq!(limiter.acquire_at(peer, now), AdmissionDecision::Allowed);
        }
        assert_eq!(limiter.tracked_peers(), MAX_TRACKED_PEERS);
        let extra = IpAddr::V4(Ipv4Addr::from(0xc000_0000 + MAX_TRACKED_PEERS as u32));
        assert_eq!(limiter.acquire_at(extra, now), AdmissionDecision::Saturated);
        assert_eq!(limiter.tracked_peers(), MAX_TRACKED_PEERS);
    }

    #[test]
    fn cleanup_removes_only_idle_fully_refilled_buckets() {
        let now = Instant::now();
        let mut limiter = limiter(1, 1, 2, 10);
        assert_eq!(limiter.acquire_at(A, now), AdmissionDecision::Allowed);
        assert_eq!(
            limiter.acquire_at(B, now + Duration::from_secs(5)),
            AdmissionDecision::Allowed
        );
        assert_eq!(limiter.prune_at(now + Duration::from_secs(9)), 0);
        assert_eq!(limiter.prune_at(now + Duration::from_secs(10)), 1);
        assert!(limiter.buckets.contains_key(&B));
        assert_eq!(limiter.prune_at(now + Duration::from_secs(15)), 1);
        assert_eq!(limiter.tracked_peers(), 0);
    }

    #[test]
    fn mapped_ipv4_uses_the_same_slot_and_ipv6_remains_independent() {
        let now = Instant::now();
        let mut limiter = limiter(1, 60, 2, 1);
        assert_eq!(limiter.acquire_at(A, now), AdmissionDecision::Allowed);
        let mapped = IpAddr::V6(Ipv4Addr::new(192, 0, 2, 1).to_ipv6_mapped());
        assert_eq!(
            limiter.acquire_at(mapped, now),
            AdmissionDecision::RateLimited {
                retry_after: Duration::from_secs(60)
            }
        );
        assert_eq!(limiter.tracked_peers(), 1);
        assert_eq!(
            limiter.acquire_at(IpAddr::V6(Ipv6Addr::LOCALHOST), now),
            AdmissionDecision::Allowed
        );
    }

    #[test]
    fn backward_clock_refuses_without_mutating_budgets_or_slots() {
        let now = Instant::now();
        let mut limiter = limiter(1, 60, 2, 1);
        assert_eq!(
            limiter.acquire_at(A, now + Duration::from_secs(1)),
            AdmissionDecision::Allowed
        );
        assert_eq!(
            limiter.acquire_at(B, now),
            AdmissionDecision::ClockUnavailable
        );
        assert_eq!(limiter.tracked_peers(), 1);
        assert_eq!(
            limiter.acquire_at(A, now + Duration::from_secs(1)),
            AdmissionDecision::RateLimited {
                retry_after: Duration::from_secs(60)
            }
        );
    }

    #[test]
    fn long_idle_refill_caps_at_capacity_without_extra_burst() {
        let now = Instant::now();
        let mut limiter = limiter(2, 1, 1, 1);
        assert_eq!(limiter.acquire_at(A, now), AdmissionDecision::Allowed);
        let later = now + Duration::from_secs(10 * 86_400);
        for _ in 0..2 {
            assert_eq!(limiter.acquire_at(A, later), AdmissionDecision::Allowed);
        }
        assert_eq!(
            limiter.acquire_at(A, later),
            AdmissionDecision::RateLimited {
                retry_after: Duration::from_millis(500)
            }
        );
    }

    #[test]
    fn policy_bounds_are_explicit_and_inclusive() {
        assert!(RatePolicy::new(1, MIN_WINDOW, 1, Duration::from_nanos(1)).is_ok());
        assert!(RatePolicy::new(MAX_BURST, MAX_WINDOW, MAX_TRACKED_PEERS, MAX_IDLE).is_ok());
        for (burst, window, peers, idle) in [
            (0, MIN_WINDOW, 1, MAX_IDLE),
            (MAX_BURST + 1, MIN_WINDOW, 1, MAX_IDLE),
            (1, MIN_WINDOW - Duration::from_nanos(1), 1, MAX_IDLE),
            (1, MAX_WINDOW + Duration::from_nanos(1), 1, MAX_IDLE),
            (1, MIN_WINDOW, 0, MAX_IDLE),
            (1, MIN_WINDOW, MAX_TRACKED_PEERS + 1, MAX_IDLE),
            (1, MIN_WINDOW, 1, Duration::ZERO),
            (1, MIN_WINDOW, 1, MAX_IDLE + Duration::from_nanos(1)),
        ] {
            assert!(RatePolicy::new(burst, window, peers, idle).is_err());
        }
        assert_eq!(InvalidRatePolicy.to_string(), "Rate policy invalid");
        assert!(std::error::Error::source(&InvalidRatePolicy).is_none());
    }

    #[test]
    fn extreme_refill_policies_have_exact_positive_retry_arithmetic() {
        let now = Instant::now();
        let mut fast =
            PeerRateLimiter::new(RatePolicy::new(MAX_BURST, MIN_WINDOW, 1, MAX_IDLE).unwrap());
        for _ in 0..MAX_BURST {
            assert_eq!(fast.acquire_at(A, now), AdmissionDecision::Allowed);
        }
        assert_eq!(
            fast.acquire_at(A, now),
            AdmissionDecision::RateLimited {
                retry_after: Duration::from_nanos(1),
            }
        );
        let next = now + Duration::from_nanos(1);
        assert_eq!(fast.acquire_at(A, next), AdmissionDecision::Allowed);
        assert_eq!(
            fast.acquire_at(A, next),
            AdmissionDecision::RateLimited {
                retry_after: Duration::from_nanos(1),
            }
        );
        let mut slow = PeerRateLimiter::new(RatePolicy::new(1, MAX_WINDOW, 1, MAX_IDLE).unwrap());
        assert_eq!(slow.acquire_at(A, now), AdmissionDecision::Allowed);
        assert_eq!(
            slow.acquire_at(A, now),
            AdmissionDecision::RateLimited {
                retry_after: MAX_WINDOW,
            }
        );
        assert_eq!(
            slow.acquire_at(A, now + MAX_WINDOW - Duration::from_nanos(1)),
            AdmissionDecision::RateLimited {
                retry_after: Duration::from_nanos(1)
            }
        );
        assert_eq!(
            slow.acquire_at(A, now + MAX_WINDOW),
            AdmissionDecision::Allowed
        );
    }

    #[test]
    fn backward_cleanup_keeps_the_budget_slot_and_clock_unchanged() {
        let mut limiter = limiter(1, 60, 1, 1);
        // Only private fixture time injection can put an observation in the future.
        let future = Instant::now() + MAX_IDLE;
        assert_eq!(limiter.acquire_at(A, future), AdmissionDecision::Allowed);
        assert_eq!(limiter.prune_idle(), 0);
        assert_eq!(limiter.observed, Some(future));
        let bucket = limiter.buckets.get(&A).unwrap();
        assert_eq!(bucket.credit, 0);
        assert_eq!(bucket.updated, future);
        assert_eq!(bucket.seen, future);
        assert_eq!(limiter.tracked_peers(), 1);
        assert_eq!(
            limiter.acquire_at(A, future),
            AdmissionDecision::RateLimited {
                retry_after: Duration::from_secs(60),
            }
        );
    }

    #[test]
    fn debug_redacts_observed_addresses() {
        let mut limiter = limiter(1, 1, 1, 1);
        assert_eq!(limiter.try_acquire(A), AdmissionDecision::Allowed);
        let text = format!("{limiter:?}");
        assert!(!text.contains("192.0.2.1"));
        assert!(text.contains("tracked_peers"));
    }
}
