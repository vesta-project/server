//! Opt-in crash restart with bounded exponential backoff and a crash-loop breaker.
//!
//! A crash is an exit the supervisor did not cause: no graceful stop, stop command
//! or kill was admitted for that generation, and the process did not exit with a
//! success status (an in-game `/stop` exits successfully and is treated as a stop).
//! Each crash is recorded with its wall-clock time. If `max_crashes` crashes fall
//! within `window`, restarting is suspended: the game is left stopped and a durable
//! suspension marker survives supervisor restarts until an operator resets it.
//! Otherwise a restart is scheduled after `initial_backoff * 2^(n-1)` (capped), where
//! `n` is the number of crashes in the window. Nothing here ever kills a process.
use crate::{Result, record};
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use vesta_state::artifacts::ArtifactDirectory;

/// Two alternating slots: a write replaces only the older one, so the newest valid
/// history survives any failed or interrupted write.
const HISTORY_SLOTS: [(&str, &str); 2] = [
    ("crash-history-a.json", "crash-history-a.staged"),
    ("crash-history-b.json", "crash-history-b.staged"),
];
const SUSPENDED: &str = "restart-suspended.json";
const SUSPENDED_STAGED: &str = "restart-suspended.staged";
const MAX_HISTORY: usize = 32;
pub const MAX_CRASH_LIMIT: u32 = 20;
pub const MAX_WINDOW: Duration = Duration::from_secs(24 * 60 * 60);
pub const MAX_BACKOFF: Duration = Duration::from_secs(60 * 60);

/// Validated, trusted host configuration. `None` at the call site means disabled.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RestartPolicy {
    max_crashes: u32,
    window: Duration,
    initial_backoff: Duration,
    max_backoff: Duration,
}
impl RestartPolicy {
    pub fn new(
        max_crashes: u32,
        window: Duration,
        initial_backoff: Duration,
        max_backoff: Duration,
    ) -> Result<Self> {
        if !(1..=MAX_CRASH_LIMIT).contains(&max_crashes)
            || window.is_zero()
            || window > MAX_WINDOW
            || initial_backoff.is_zero()
            || initial_backoff > max_backoff
            || max_backoff > MAX_BACKOFF
        {
            return Err("supervisor_restart_policy_invalid");
        }
        Ok(Self {
            max_crashes,
            window,
            initial_backoff,
            max_backoff,
        })
    }
    pub fn to_wire(self) -> crate::wire::WireRestartPolicy {
        crate::wire::WireRestartPolicy {
            crash_limit: self.max_crashes,
            window_secs: self.window.as_secs(),
            backoff_secs: self.initial_backoff.as_secs(),
            max_backoff_secs: self.max_backoff.as_secs(),
        }
    }
    pub fn from_wire(wire: crate::wire::WireRestartPolicy) -> Result<Self> {
        Self::new(
            wire.crash_limit,
            Duration::from_secs(wire.window_secs),
            Duration::from_secs(wire.backoff_secs),
            Duration::from_secs(wire.max_backoff_secs),
        )
    }
    /// The recommended preset: three crashes within ten minutes, 10 s to 5 min backoff.
    pub fn recommended() -> Self {
        Self {
            max_crashes: 3,
            window: Duration::from_secs(600),
            initial_backoff: Duration::from_secs(10),
            max_backoff: Duration::from_secs(300),
        }
    }
    fn backoff(&self, crashes: u32) -> Duration {
        let factor = 1u32
            .checked_shl(crashes.saturating_sub(1).min(16))
            .unwrap_or(u32::MAX);
        self.initial_backoff
            .checked_mul(factor)
            .unwrap_or(self.max_backoff)
            .min(self.max_backoff)
    }
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct History {
    version: u8,
    seq: u64,
    crashes: Vec<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Suspension {
    version: u8,
    suspended_at: i64,
    crashes: u32,
}

/// What the supervisor should do after a crash.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Decision {
    Disabled,
    RestartAfter(Duration),
    Suspended,
}

pub struct Restarts {
    policy: Option<RestartPolicy>,
    directory: ArtifactDirectory,
    history: Vec<i64>,
    /// Sequence and slot of the newest durable history.
    seq: u64,
    slot: usize,
    suspended: bool,
    /// The suspension is in force but its marker is not yet durable; retried.
    unsaved: Option<Suspension>,
    pending: Option<Instant>,
}

pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

impl Restarts {
    /// Load durable crash state. A malformed suspension marker refuses. Unreadable or
    /// corrupt history fails closed as a suspension rather than reading as no crashes.
    pub fn load(directory: ArtifactDirectory, policy: Option<RestartPolicy>) -> Result<Self> {
        let mut suspended = record::read_json::<Suspension>(&directory, SUSPENDED)?.is_some();
        let mut newest: Option<(usize, History)> = None;
        for (slot, (name, _)) in HISTORY_SLOTS.iter().enumerate() {
            match record::read_json::<History>(&directory, name) {
                Ok(Some(history)) if history.version == 1 => {
                    if newest.as_ref().is_none_or(|(_, n)| history.seq > n.seq) {
                        newest = Some((slot, history));
                    }
                }
                Ok(None) => {}
                _ => suspended = true,
            }
        }
        let (slot, history) = newest.unwrap_or((
            1,
            History {
                version: 1,
                seq: 0,
                crashes: Vec::new(),
            },
        ));
        let mut restarts = Self {
            policy,
            directory,
            history: history.crashes,
            seq: history.seq,
            slot,
            suspended,
            unsaved: None,
            pending: None,
        };
        if suspended {
            restarts.unsaved = Some(Suspension {
                version: 1,
                suspended_at: now_ms(),
                crashes: restarts.history.len() as u32,
            });
            restarts.retry_persist();
        }
        Ok(restarts)
    }

    /// Apply the agent's current trusted policy. Disabling cancels a scheduled restart;
    /// a tripped breaker stays tripped until an explicit reset.
    pub fn set_policy(&mut self, policy: Option<RestartPolicy>) {
        if policy.is_none() {
            self.pending = None;
        }
        self.policy = policy;
    }

    pub fn enabled(&self) -> bool {
        self.policy.is_some()
    }
    pub fn suspended(&self) -> bool {
        self.suspended
    }
    /// The breaker is in force in memory but its durable marker is not yet written.
    pub fn unsaved(&self) -> bool {
        self.unsaved.is_some()
    }
    /// Time until a scheduled automatic restart, if any.
    pub fn pending_in(&self, now: Instant) -> Option<Duration> {
        self.pending.map(|at| at.saturating_duration_since(now))
    }
    pub fn recent_crashes(&self, now_ms: i64) -> u32 {
        match self.policy {
            Some(policy) => self.in_window(&policy, now_ms),
            None => 0,
        }
    }
    fn in_window(&self, policy: &RestartPolicy, now_ms: i64) -> u32 {
        let window = i64::try_from(policy.window.as_millis()).unwrap_or(i64::MAX);
        self.history
            .iter()
            .filter(|&&at| now_ms.saturating_sub(at) < window)
            .count() as u32
    }

    /// Record an unexpected exit and decide what to do. The breaker never fails open:
    /// if its marker cannot be written it stays tripped in memory and is retried.
    pub fn on_crash(&mut self, now: Instant, now_ms: i64) -> Decision {
        let Some(policy) = self.policy else {
            return Decision::Disabled;
        };
        self.history.push(now_ms);
        let window = i64::try_from(policy.window.as_millis()).unwrap_or(i64::MAX);
        self.history
            .retain(|&at| now_ms.saturating_sub(at) < window);
        if self.history.len() > MAX_HISTORY {
            let excess = self.history.len() - MAX_HISTORY;
            self.history.drain(..excess);
        }
        self.persist_history();
        let crashes = self.history.len() as u32;
        if self.suspended || crashes >= policy.max_crashes {
            self.pending = None;
            if !self.suspended {
                self.suspended = true;
                self.unsaved = Some(Suspension {
                    version: 1,
                    suspended_at: now_ms,
                    crashes,
                });
                self.retry_persist();
            }
            return Decision::Suspended;
        }
        let delay = policy.backoff(crashes);
        self.pending = now.checked_add(delay);
        Decision::RestartAfter(delay)
    }

    /// Write a pending suspension marker. Called after a trip and on every observation.
    pub fn retry_persist(&mut self) {
        let Some(suspension) = &self.unsaved else {
            return;
        };
        match record::publish(&self.directory, SUSPENDED_STAGED, SUSPENDED, suspension) {
            Ok(()) | Err("supervisor_record_exists") => self.unsaved = None,
            Err(_) => {}
        }
    }

    /// Take a due automatic restart. It is consumed whether or not the launch works.
    pub fn take_due(&mut self, now: Instant) -> bool {
        if self.pending.is_some_and(|at| now >= at) && !self.suspended && self.policy.is_some() {
            self.pending = None;
            return true;
        }
        false
    }

    /// An explicit start or stop supersedes a scheduled automatic restart.
    pub fn cancel_pending(&mut self) {
        self.pending = None;
    }

    /// Explicit operator reset: clear suspension, history (including any corrupt slot)
    /// and any scheduled restart.
    pub fn reset(&mut self) -> Result<()> {
        self.directory
            .remove(&record::name(SUSPENDED)?)
            .map_err(|_| "supervisor_record_unavailable")?;
        for (name, _) in HISTORY_SLOTS {
            self.directory
                .remove(&record::name(name)?)
                .map_err(|_| "supervisor_record_unavailable")?;
        }
        self.suspended = false;
        self.unsaved = None;
        self.history.clear();
        self.pending = None;
        self.persist_history();
        Ok(())
    }

    fn persist_history(&mut self) {
        // Write the older slot only; the newest durable history stays intact if this
        // fails. History shapes backoff and the window; suspension has its own marker.
        let target = 1 - self.slot;
        let (name, staged) = HISTORY_SLOTS[target];
        let Ok(published) = record::name(name) else {
            return;
        };
        let seq = self.seq.saturating_add(1);
        if self.directory.remove(&published).is_ok()
            && record::publish(
                &self.directory,
                staged,
                name,
                &History {
                    version: 1,
                    seq,
                    crashes: self.history.clone(),
                },
            )
            .is_ok()
        {
            self.seq = seq;
            self.slot = target;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vesta_state::StateDirectory;

    fn directory() -> (tempfile::TempDir, ArtifactDirectory) {
        let temp = tempfile::tempdir().unwrap();
        let state = StateDirectory::new(temp.path().join("state"), None).unwrap();
        state.prepare().unwrap();
        let directory = state.supervisor().unwrap();
        (temp, directory)
    }

    #[test]
    fn policy_bounds_are_enforced() {
        let s = Duration::from_secs;
        assert!(RestartPolicy::new(0, s(600), s(10), s(300)).is_err());
        assert!(RestartPolicy::new(21, s(600), s(10), s(300)).is_err());
        assert!(RestartPolicy::new(3, s(0), s(10), s(300)).is_err());
        assert!(RestartPolicy::new(3, s(600), s(0), s(300)).is_err());
        assert!(RestartPolicy::new(3, s(600), s(400), s(300)).is_err());
        assert!(RestartPolicy::new(3, s(600), s(10), s(7200)).is_err());
        assert_eq!(
            RestartPolicy::new(3, s(600), s(10), s(300)).unwrap(),
            RestartPolicy::recommended()
        );
    }

    #[test]
    fn backoff_doubles_then_breaker_suspends_durably_until_reset() {
        let (_temp, directory) = directory();
        let mut restarts =
            Restarts::load(directory.clone(), Some(RestartPolicy::recommended())).unwrap();
        let now = Instant::now();
        let t = 1_800_000_000_000;
        assert_eq!(
            restarts.on_crash(now, t),
            Decision::RestartAfter(Duration::from_secs(10))
        );
        assert!(!restarts.take_due(now));
        assert!(restarts.take_due(now + Duration::from_secs(10)));
        assert!(!restarts.take_due(now + Duration::from_secs(11)));
        assert_eq!(
            restarts.on_crash(now, t + 1_000),
            Decision::RestartAfter(Duration::from_secs(20))
        );
        assert_eq!(restarts.on_crash(now, t + 2_000), Decision::Suspended);
        assert!(restarts.suspended());
        assert_eq!(restarts.pending_in(now), None);
        // Survives a supervisor restart.
        let mut reloaded =
            Restarts::load(directory.clone(), Some(RestartPolicy::recommended())).unwrap();
        assert!(reloaded.suspended());
        assert_eq!(reloaded.recent_crashes(t + 3_000), 3);
        assert_eq!(reloaded.on_crash(now, t + 4_000), Decision::Suspended);
        reloaded.reset().unwrap();
        assert!(!reloaded.suspended());
        let fresh = Restarts::load(directory, Some(RestartPolicy::recommended())).unwrap();
        assert!(!fresh.suspended());
        assert_eq!(fresh.recent_crashes(t + 5_000), 0);
    }

    #[test]
    fn crashes_outside_the_window_do_not_count_and_disabled_never_restarts() {
        let (_temp, directory) = directory();
        let mut restarts =
            Restarts::load(directory.clone(), Some(RestartPolicy::recommended())).unwrap();
        let now = Instant::now();
        let t = 1_800_000_000_000;
        restarts.on_crash(now, t);
        restarts.on_crash(now, t + 1);
        // Eleven minutes later the earlier two have aged out of the ten-minute window.
        assert_eq!(
            restarts.on_crash(now, t + 660_000),
            Decision::RestartAfter(Duration::from_secs(10))
        );
        let mut disabled = Restarts::load(directory, None).unwrap();
        assert_eq!(disabled.on_crash(now, t), Decision::Disabled);
        assert!(!disabled.take_due(now + Duration::from_secs(3600)));
    }

    #[test]
    fn corrupt_history_fails_closed_and_newest_slot_wins() {
        use std::io::Write;
        let (_temp, directory) = directory();
        let now = Instant::now();
        let mut restarts =
            Restarts::load(directory.clone(), Some(RestartPolicy::recommended())).unwrap();
        restarts.on_crash(now, 1_800_000_000_000);
        restarts.on_crash(now, 1_800_000_000_001);
        // Both slots now hold history; the newer one has two crashes.
        let reloaded =
            Restarts::load(directory.clone(), Some(RestartPolicy::recommended())).unwrap();
        assert_eq!(reloaded.recent_crashes(1_800_000_000_002), 2);
        assert!(!reloaded.suspended());
        // Corrupting a slot is never read as "no crashes": the breaker trips durably.
        let name = record::name(HISTORY_SLOTS[0].0).unwrap();
        directory.remove(&name).unwrap();
        directory
            .create_new(&name)
            .unwrap()
            .write_all(b"{corrupt")
            .unwrap();
        let mut corrupt =
            Restarts::load(directory.clone(), Some(RestartPolicy::recommended())).unwrap();
        assert!(corrupt.suspended());
        assert!(!corrupt.unsaved());
        assert!(Restarts::load(directory.clone(), None).unwrap().suspended());
        corrupt.reset().unwrap();
        let clean = Restarts::load(directory, Some(RestartPolicy::recommended())).unwrap();
        assert!(!clean.suspended());
        assert_eq!(clean.recent_crashes(1_800_000_000_002), 0);
    }

    #[test]
    fn disabling_the_policy_cancels_a_scheduled_restart() {
        let (_temp, directory) = directory();
        let mut restarts = Restarts::load(directory, Some(RestartPolicy::recommended())).unwrap();
        let now = Instant::now();
        restarts.on_crash(now, 1_800_000_000_000);
        restarts.set_policy(None);
        assert_eq!(restarts.pending_in(now), None);
        assert!(!restarts.take_due(now + Duration::from_secs(3600)));
    }

    #[test]
    fn explicit_actions_cancel_a_scheduled_restart() {
        let (_temp, directory) = directory();
        let mut restarts = Restarts::load(directory, Some(RestartPolicy::recommended())).unwrap();
        let now = Instant::now();
        restarts.on_crash(now, 1_800_000_000_000);
        restarts.cancel_pending();
        assert!(!restarts.take_due(now + Duration::from_secs(3600)));
    }
}
