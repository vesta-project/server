//! Opt-in sleep and network wake use the normal durable operation owner. Unknown
//! player evidence is a deferral, never an empty server. Socket ownership lives
//! with the actual game owner, including the independent supervisor.
use crate::{
    identity::{self, Capability, Decision, DeviceId, PrincipalId, Subject},
    motd::{self, State},
    operations::{
        GameCommand, GenerationId, OperationState, ProcessPhase, RuntimeHandle, StartGate,
    },
};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::{
    net::SocketAddr,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
use vesta_state::{
    StateDatabase, StateError,
    audit::{AuditActor, AuditLog, AuditRecord},
};

const POLICY_KEY: &str = "sleep-policy";
const RUNTIME_KEY: &str = "sleep-runtime";
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    pub enabled: bool,
    pub offline_status: bool,
    pub wake_on_join: bool,
    pub idle_seconds: u32,
    pub warning_seconds: u32,
    pub start_cooldown_seconds: u32,
}
impl Default for Policy {
    fn default() -> Self {
        Self {
            enabled: false,
            offline_status: false,
            wake_on_join: false,
            idle_seconds: 600,
            warning_seconds: 30,
            start_cooldown_seconds: 30,
        }
    }
}
impl Policy {
    pub fn valid(&self) -> bool {
        (60..=86400).contains(&self.idle_seconds)
            && (5..=300).contains(&self.warning_seconds)
            && (10..=3600).contains(&self.start_cooldown_seconds)
            && (!self.wake_on_join || self.offline_status)
    }
}
#[derive(Clone, Serialize)]
pub struct PolicyView {
    pub revision: i64,
    pub policy: Policy,
    #[serde(rename = "game_port_known")]
    pub port_available: bool,
    pub sleep_blocker: SleepBlocker,
}
/// Operational explanation only; never changes the canonical process/lifecycle state.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum SleepBlocker {
    Disabled,
    #[default]
    NotReady,
    PlayersOnline,
    PlayerCountUnknown,
    OperationInProgress,
    Hold,
    AuthorityUnavailable,
    CountingDown {
        remaining_seconds: u32,
    },
    WarningSent {
        remaining_seconds: u32,
    },
}
#[derive(Clone)]
struct Diagnostic {
    revision: i64,
    blocker: SleepBlocker,
}
#[derive(Serialize, Deserialize)]
struct StoredPolicy {
    policy: Policy,
    principal: String,
    device: String,
}
impl StoredPolicy {
    fn subject(&self) -> Result<Subject, &'static str> {
        Ok(Subject {
            principal: PrincipalId::parse(&self.principal).map_err(|_| "sleep_policy_invalid")?,
            device: DeviceId::parse(&self.device).map_err(|_| "sleep_policy_invalid")?,
        })
    }
}
#[derive(Default, Serialize, Deserialize)]
struct Record {
    sleeping: bool,
    sleep_key: Option<String>,
    wake_key: Option<String>,
    cooldown_until_ms: i64,
    sequence: u64,
}
#[derive(Clone)]
pub struct Sleep {
    db: StateDatabase,
    gate: StartGate,
    address: Option<SocketAddr>,
    probe: Option<SocketAddr>,
    server_name: String,
    minecraft_version: String,
    pack_version: String,
    loader_version: String,
    observed: Arc<Mutex<Option<ObservedState>>>,
    diagnostic: Arc<Mutex<Option<Diagnostic>>>,
}
struct ObservedState {
    state: State,
    generation: Option<GenerationId>,
    since: Instant,
}
impl Sleep {
    pub(crate) fn new(
        db: StateDatabase,
        gate: StartGate,
        address: Option<SocketAddr>,
        probe: Option<SocketAddr>,
        server_name: String,
    ) -> Self {
        Self {
            db,
            gate,
            address,
            probe,
            server_name,
            minecraft_version: "unknown".into(),
            pack_version: "unknown".into(),
            loader_version: "unknown".into(),
            observed: Arc::default(),
            diagnostic: Arc::default(),
        }
    }
    pub(crate) fn with_metadata(mut self, minecraft: &str, pack: &str, loader: &str) -> Self {
        self.minecraft_version = minecraft.into();
        self.pack_version = pack.into();
        self.loader_version = loader.into();
        self
    }
    pub fn policy(&self, subject: &Subject) -> Result<PolicyView, &'static str> {
        self.db
            .read(|c| {
                authorize(c, &self.db, subject, &[Capability::ConfigView])?;
                let (revision, stored) = load_policy(c, &self.db)?;
                let policy = stored.map(|s| s.policy).unwrap_or_default();
                Ok(PolicyView {
                    revision,
                    sleep_blocker: self.diagnostic_for(revision, &policy),
                    policy,
                    port_available: self.address.is_some(),
                })
            })
            .map_err(map_state_error)
    }
    pub fn configure(
        &self,
        subject: &Subject,
        expected_revision: i64,
        policy: Policy,
    ) -> Result<PolicyView, &'static str> {
        if !policy.valid() || ((policy.enabled || policy.offline_status) && self.address.is_none())
        {
            return Err("sleep_policy_invalid");
        }
        let audit = AuditLog::new(self.db.clone()).map_err(map_state_error)?;
        self.db
            .write(|c| {
                let mut required = vec![Capability::ConfigEdit];
                if policy.enabled {
                    required.extend([
                        Capability::LifecycleStop,
                        Capability::ConsoleRead,
                        Capability::ConsoleWrite,
                        Capability::PlayersView,
                    ]);
                }
                if policy.wake_on_join {
                    required.push(Capability::LifecycleStart);
                }
                authorize(c, &self.db, subject, &required)?;
                let (revision, _) = load_policy(c, &self.db)?;
                if revision != expected_revision {
                    return Err(StateError::new("state_sleep_policy_stale_revision"));
                }
                let stored = StoredPolicy {
                    policy: policy.clone(),
                    principal: subject.principal.as_str().into(),
                    device: subject.device.as_str().into(),
                };
                put(c, &self.db, POLICY_KEY, &stored)?;
                // A new policy never inherits an old actor's queued intent.
                // Keep cooldown/sequence so configuration cannot bypass them.
                let mut record = load_record(c, &self.db)?;
                record.sleep_key = None;
                record.wake_key = None;
                put(c, &self.db, RUNTIME_KEY, &record)?;
                audit.append(
                    c,
                    &AuditRecord::of(
                        AuditActor::new(stored.principal, stored.device, "sleep-policy".into())?,
                        "sleep.policy.configure".into(),
                        "configured-game".into(),
                        "ok".into(),
                        None,
                    )?,
                )?;
                Ok(PolicyView {
                    revision: revision + 1,
                    sleep_blocker: self.diagnostic_for(revision + 1, &policy),
                    policy,
                    port_available: self.address.is_some(),
                })
            })
            .map_err(map_state_error)
    }
    pub fn has_lifecycle_intent(
        &self,
        runtime: &RuntimeHandle,
        subject: &Subject,
        key: &str,
    ) -> Result<bool, &'static str> {
        runtime
            .operations_service()
            .find_key_trusted(&subject.principal, key)
            .map(|v| v.is_some())
            .map_err(|_| "sleep_state_unavailable")
    }
    pub fn available(&self) -> bool {
        self.address.is_some()
            && self
                .db
                .read(|c| load_policy(c, &self.db))
                .ok()
                .and_then(|(_, p)| p)
                .is_some_and(|p| p.policy.offline_status)
    }
    pub(crate) fn initialize(
        &self,
        policy: crate::config::OnAgentStart,
    ) -> Result<(), &'static str> {
        let sleeping = policy == crate::config::OnAgentStart::Sleep;
        if sleeping && !self.available() {
            return Err("sleep_listener_not_configured");
        }
        self.db
            .write(|c| {
                lifecycle_finished_in(
                    c,
                    self.db.server_id(),
                    self.db.clock().millis(),
                    if sleeping {
                        "game.manual_sleep"
                    } else {
                        "game.stop_sleeping"
                    },
                )
            })
            .map_err(|_| "sleep_state_unavailable")
    }
    pub fn public_state(&self, runtime: &RuntimeHandle) -> Result<State, &'static str> {
        let state = self.current_state(runtime)?;
        let generation = runtime
            .game_snapshot()
            .ok()
            .and_then(|snapshot| snapshot.generation);
        let _ = self.observed_elapsed_at(state, generation, Instant::now())?;
        Ok(state)
    }
    fn diagnostic_for(&self, revision: i64, policy: &Policy) -> SleepBlocker {
        if !policy.enabled {
            return SleepBlocker::Disabled;
        }
        self.diagnostic
            .lock()
            .ok()
            .and_then(|d| {
                d.as_ref()
                    .filter(|d| d.revision == revision)
                    .map(|d| d.blocker.clone())
            })
            .unwrap_or_default()
    }
    pub fn sleep_blocker(&self) -> Result<SleepBlocker, &'static str> {
        self.db
            .read(|c| {
                let (revision, stored) = load_policy(c, &self.db)?;
                Ok(self.diagnostic_for(revision, &stored.map(|s| s.policy).unwrap_or_default()))
            })
            .map_err(map_state_error)
    }
    /// Time since this running Vesta agent observed the state/generation pair.
    /// Prior offline duration is unknown and deliberately is not reconstructed.
    pub(crate) fn observed_elapsed_at(
        &self,
        state: State,
        generation: Option<GenerationId>,
        now: Instant,
    ) -> Result<u64, &'static str> {
        let mut observed = self
            .observed
            .lock()
            .map_err(|_| "sleep_state_unavailable")?;
        if observed
            .as_ref()
            .is_none_or(|old| old.state != state || old.generation != generation)
        {
            *observed = Some(ObservedState {
                state,
                generation,
                since: now,
            });
        }
        Ok(observed
            .as_ref()
            .map(|old| now.saturating_duration_since(old.since).as_secs())
            .unwrap_or(0))
    }
    fn current_state(&self, runtime: &RuntimeHandle) -> Result<State, &'static str> {
        if runtime.source_revision().is_err() {
            return Ok(State::RecoveryRequired);
        }
        if let Some(health) = runtime
            .supervision()
            .map_err(|_| "sleep_state_unavailable")?
        {
            if health.crash_loop {
                return Ok(State::CrashLoop);
            }
            if health.orphan_running
                || health.launch_unresolved
                || health.observation_failed
                || health.save_state_unknown
                || health.save_lease_stranded
            {
                return Ok(State::RecoveryRequired);
            }
        }
        let recovery = self.db.read(|c| c.query_row("SELECT EXISTS(SELECT 1 FROM operations WHERE server_id=? AND state='recovery_required')",[self.db.server_id()],|r|r.get::<_,bool>(0)).map_err(Into::into)).map_err(|_|"sleep_state_unavailable")?;
        if recovery {
            return Ok(State::RecoveryRequired);
        }
        let snapshot = runtime
            .game_snapshot()
            .map_err(|_| "sleep_state_unavailable")?;
        if matches!(
            snapshot.phase,
            ProcessPhase::Stopping | ProcessPhase::Draining
        ) {
            return Ok(State::Stopping);
        }
        if snapshot.generation.is_some() {
            let readiness = runtime.readiness().map_err(|_| "sleep_state_unavailable")?;
            return Ok(
                if matches!(
                    readiness.readiness,
                    crate::operations::readiness::Readiness::Ready
                ) {
                    State::Online
                } else {
                    State::Starting
                },
            );
        }
        // The existing gate is an exclusion for stopped backups, not a first-class
        // maintenance admission overlay. Do not mislabel it as maintenance.
        let record = self
            .db
            .read(|c| load_record(c, &self.db))
            .map_err(|_| "sleep_state_unavailable")?;
        if record.wake_key.is_some() {
            Ok(State::Waking)
        } else if record.sleeping {
            Ok(State::Sleeping)
        } else {
            Ok(State::Offline)
        }
    }
    pub fn public_presentation(
        &self,
        runtime: &RuntimeHandle,
    ) -> Result<motd::Rendered, &'static str> {
        let state = self.public_state(runtime)?;
        let document = self.document()?;
        document
            .render(state, &self.public_inputs(runtime, state)?)
            .map_err(|_| "motd_invalid")
    }
    fn document(&self) -> Result<motd::Document, &'static str> {
        self.db
            .read(|c| load::<motd::Document>(c, &self.db, "motd-policy"))
            .map_err(|_| "motd_state_unavailable")
            .map(|row| row.map(|(_, d)| d).unwrap_or_default())
    }
    fn public_inputs<'a>(
        &'a self,
        runtime: &RuntimeHandle,
        state: State,
    ) -> Result<motd::PublicInputs<'a>, &'static str> {
        let ready = runtime.readiness().map_err(|_| "sleep_state_unavailable")?;
        let generation = runtime
            .game_snapshot()
            .map_err(|_| "sleep_state_unavailable")?
            .generation;
        let elapsed_seconds = self.observed_elapsed_at(state, generation, Instant::now())?;
        Ok(motd::PublicInputs {
            server_name: &self.server_name,
            minecraft_version: &self.minecraft_version,
            pack_name: &self.server_name,
            pack_version: &self.pack_version,
            loader_version: &self.loader_version,
            elapsed_seconds,
            online_players: if state == State::Online {
                ready.players.and_then(|(n, _)| u16::try_from(n).ok())
            } else {
                Some(0)
            },
            queued_operation: match state {
                State::Waking | State::Starting => Some(motd::PublicOperation::Start),
                State::Stopping => Some(motd::PublicOperation::Stop),
                _ => None,
            },
            public_failure: match state {
                State::CrashLoop => Some(motd::PublicFailure::CrashLoop),
                State::RecoveryRequired => Some(motd::PublicFailure::RecoveryRequired),
                _ => None,
            },
            ..Default::default()
        })
    }
    pub(crate) fn start_driver(
        &self,
        cell: Arc<std::sync::OnceLock<RuntimeHandle>>,
    ) -> Result<Driver, &'static str> {
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let service = self.clone();
        let worker = thread::Builder::new()
            .name("vesta-sleep".into())
            .spawn(move || {
                let mut coordinator = Coordinator::default();
                while !flag.load(Ordering::Acquire) {
                    if let Some(runtime) = cell.get()
                        && coordinator.tick(&service, runtime).is_err()
                    {
                        let _ = runtime.configure_offline(None);
                        coordinator.reset();
                    }
                    thread::park_timeout(coordinator.poll_interval());
                }
                if let Some(runtime) = cell.get() {
                    let _ = runtime.configure_offline(None);
                }
            })
            .map_err(|_| "sleep_driver_unavailable")?;
        Ok(Driver {
            stop,
            worker: Some(worker),
        })
    }
}
pub(crate) struct Driver {
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}
impl Drop for Driver {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            worker.thread().unpark();
            let _ = worker.join();
        }
    }
}

/// Same transaction as sleep's actual native effect gate. Pending and uncertain
/// work is excluded; a timer or lost owner cannot waive it.
pub(crate) fn blocked_in(
    c: &Connection,
    server: &str,
    exclude: Option<&str>,
) -> vesta_state::Result<bool> {
    c.query_row("SELECT EXISTS(SELECT 1 FROM operations WHERE server_id=?1 AND (?2 IS NULL OR id<>?2) AND state NOT IN ('succeeded','failed','cancelled')) OR EXISTS(SELECT 1 FROM backup_jobs WHERE state IN ('queued','running','uncertain')) OR EXISTS(SELECT 1 FROM file_intents WHERE server_id=?1 AND status IN ('pending','recovery-required'))",params![server,exclude],|r|r.get(0)).map_err(Into::into)
}
pub(crate) fn permit_operation_in(
    c: &Connection,
    server: &str,
    subject: &Subject,
    key: Option<&str>,
    wake: bool,
) -> vesta_state::Result<()> {
    let value: Option<String> = c
        .query_row(
            "SELECT desired_value FROM settings WHERE server_id=? AND key=?",
            params![server, POLICY_KEY],
            |r| r.get(0),
        )
        .optional()?;
    let stored: StoredPolicy = serde_json::from_str(
        &value.ok_or_else(|| StateError::new("state_sleep_policy_unavailable"))?,
    )
    .map_err(|_| StateError::new("state_sleep_policy_invalid"))?;
    if !stored.policy.valid()
        || stored.principal != subject.principal.as_str()
        || stored.device != subject.device.as_str()
        || if wake {
            !stored.policy.wake_on_join
        } else {
            !stored.policy.enabled
        }
    {
        return Err(StateError::new("state_sleep_policy_changed"));
    }
    let value: String = c.query_row(
        "SELECT desired_value FROM settings WHERE server_id=? AND key=?",
        params![server, RUNTIME_KEY],
        |r| r.get(0),
    )?;
    let record: Record =
        serde_json::from_str(&value).map_err(|_| StateError::new("state_sleep_state_invalid"))?;
    if wake && !record.sleeping {
        return Err(StateError::new("state_sleep_policy_changed"));
    }
    let expected = if wake {
        record.wake_key.as_deref()
    } else {
        record.sleep_key.as_deref()
    };
    if expected != key || key.is_none() {
        return Err(StateError::new("state_sleep_policy_changed"));
    }
    Ok(())
}
pub(crate) fn bind_from_properties(text: &str) -> Option<SocketAddr> {
    crate::config::game_bind_address(text.as_bytes())
}
fn map_state_error(error: StateError) -> &'static str {
    match error.code() {
        "state_sleep_forbidden" => "sleep_forbidden",
        "state_sleep_policy_stale_revision" => "sleep_policy_stale_revision",
        _ => "sleep_policy_unavailable",
    }
}
fn authorize(
    c: &Connection,
    db: &StateDatabase,
    subject: &Subject,
    caps: &[Capability],
) -> vesta_state::Result<()> {
    let policy = identity::policy_in(c, db.server_id(), db.clock().millis(), Some(subject))
        .map_err(|error| match error {
            crate::ControlError::State(error) => error,
            _ => StateError::new("state_sleep_policy_unavailable"),
        })?;
    if caps
        .iter()
        .any(|cap| policy.check(*cap, None) != Decision::Allow)
    {
        return Err(StateError::new("state_sleep_forbidden"));
    }
    Ok(())
}
fn load<T: serde::de::DeserializeOwned>(
    c: &Connection,
    db: &StateDatabase,
    key: &str,
) -> vesta_state::Result<Option<(i64, T)>> {
    let row = c
        .query_row(
            "SELECT desired_revision,desired_value FROM settings WHERE server_id=? AND key=?",
            params![db.server_id(), key],
            |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)),
        )
        .optional()?;
    row.map(|(revision, value)| {
        serde_json::from_str(&value)
            .map(|v| (revision, v))
            .map_err(|_| StateError::new("state_sleep_state_invalid"))
    })
    .transpose()
}
fn load_policy(
    c: &Connection,
    db: &StateDatabase,
) -> vesta_state::Result<(i64, Option<StoredPolicy>)> {
    match load::<StoredPolicy>(c, db, POLICY_KEY)? {
        Some((revision, stored)) if stored.policy.valid() => Ok((revision, Some(stored))),
        Some(_) => Err(StateError::new("state_sleep_state_invalid")),
        None => Ok((0, None)),
    }
}
fn load_record(c: &Connection, db: &StateDatabase) -> vesta_state::Result<Record> {
    Ok(load(c, db, RUNTIME_KEY)?
        .map(|(_, r)| r)
        .unwrap_or_default())
}
fn put<T: Serialize>(
    c: &Connection,
    db: &StateDatabase,
    key: &str,
    value: &T,
) -> vesta_state::Result<()> {
    let value =
        serde_json::to_string(value).map_err(|_| StateError::new("state_sleep_state_invalid"))?;
    if value.len() > 4096 {
        return Err(StateError::new("state_sleep_state_invalid"));
    }
    c.execute("INSERT INTO settings(server_id,key,desired_value,desired_revision,apply_mode,effective_value,effective_revision,updated_at) VALUES(?,?,?,1,'immediate',?,1,?) ON CONFLICT(server_id,key) DO UPDATE SET desired_value=excluded.desired_value,desired_revision=settings.desired_revision+1,effective_value=excluded.desired_value,effective_revision=settings.desired_revision+1,updated_at=excluded.updated_at",params![db.server_id(),key,value,value,db.clock().millis()])?;
    Ok(())
}

#[derive(Default)]
pub(crate) struct Coordinator {
    fast_poll: bool,
    generation: Option<GenerationId>,
    revision: i64,
    zero_since: Option<Instant>,
    warned_at: Option<Instant>,
    cursor: Option<u64>,
    sampled: Option<Instant>,
    warning_id: Option<String>,
    warning_text: Option<String>,
    blocker: SleepBlocker,
}
impl Coordinator {
    pub(crate) fn poll_interval(&self) -> Duration {
        if self.fast_poll {
            Duration::from_millis(250)
        } else {
            Duration::from_secs(1)
        }
    }
    fn reset(&mut self) {
        self.warning_id = None;
        self.warning_text = None;
        self.zero_since = None;
        self.warned_at = None;
        self.cursor = None;
        // Keep pacing across failed observations; reset must not retry list every tick.
    }
    fn tick(&mut self, service: &Sleep, runtime: &RuntimeHandle) -> Result<(), &'static str> {
        self.tick_at(service, runtime, Instant::now())
    }
    pub(crate) fn tick_at(
        &mut self,
        service: &Sleep,
        runtime: &RuntimeHandle,
        now: Instant,
    ) -> Result<(), &'static str> {
        let result = self.evaluate_at(service, runtime, now);
        if result.is_err() {
            self.blocker = SleepBlocker::PlayerCountUnknown;
        }
        if let Ok(mut diagnostic) = service.diagnostic.lock() {
            *diagnostic = Some(Diagnostic {
                revision: self.revision,
                blocker: self.blocker.clone(),
            });
        }
        result
    }
    fn evaluate_at(
        &mut self,
        service: &Sleep,
        runtime: &RuntimeHandle,
        now: Instant,
    ) -> Result<(), &'static str> {
        // No active sleep/offline feature: one bounded policy observation per
        // second rather than four. Active wake/countdown cadence is unchanged.
        self.fast_poll = false;
        let (revision, stored) = service
            .db
            .read(|c| load_policy(c, &service.db))
            .map_err(|_| "sleep_policy_unavailable")?;
        let Some(stored) = stored else {
            self.blocker = SleepBlocker::Disabled;
            runtime.configure_offline(None)?;
            self.reset();
            return Ok(());
        };
        self.fast_poll = stored.policy.enabled || stored.policy.offline_status;
        let subject = stored.subject()?;
        let snapshot = runtime
            .game_snapshot()
            .map_err(|_| "sleep_state_unavailable")?;
        if self.generation != snapshot.generation || self.revision != revision {
            self.reset();
            self.generation = snapshot.generation;
            self.revision = revision;
        }
        self.reconcile(service, runtime, &subject, snapshot.generation)?;
        let state = service.public_state(runtime)?;
        let healthy = !matches!(state, State::CrashLoop | State::RecoveryRequired);
        let blocked = service.gate.blocked()
            || service
                .db
                .read(|c| blocked_in(c, service.db.server_id(), self.warning_id.as_deref()))
                .map_err(|_| "sleep_state_unavailable")?;
        let wake_authorized = service
            .db
            .read(|c| authorize(c, &service.db, &subject, &[Capability::LifecycleStart]))
            .is_ok();
        let wakes = if stored.policy.offline_status
            && snapshot.generation.is_none()
            && matches!(state, State::Sleeping | State::Waking)
        {
            let document = service.document()?;
            let inputs = service.public_inputs(runtime, state)?;
            let rendered = document
                .render(state, &inputs)
                .map_err(|_| "motd_invalid")?;
            let status = serde_json::json!({"version":{"name":rendered.version_name,"protocol":-1},"players":{"online":0,"max":rendered.maximum_players},"description":rendered.description,"favicon":rendered.favicon_data_uri});
            let can_wake = state == State::Sleeping
                && stored.policy.wake_on_join
                && healthy
                && !blocked
                && wake_authorized;
            let disconnect = if can_wake || matches!(state, State::Waking | State::Starting) {
                document
                    .disconnect(state, &inputs)
                    .map_err(|_| "motd_invalid")?
            } else if matches!(
                state,
                State::CrashLoop | State::RecoveryRequired | State::Stopping
            ) {
                rendered.description.clone()
            } else {
                serde_json::json!({"text":format!("Server is {}. Network wake is unavailable.",state.name())})
            };
            runtime.configure_offline(Some(vesta_game_runtime::offline::Configuration {
                address: service.address.ok_or("sleep_port_unknown")?,
                status_json: status.to_string(),
                disconnect: disconnect.to_string(),
                wake: can_wake,
            }))?
        } else {
            runtime.configure_offline(None)?
        };
        let pending_wake = service
            .db
            .read(|c| load_record(c, &service.db))
            .map_err(|_| "sleep_state_unavailable")?
            .wake_key
            .is_some();
        if (!wakes.is_empty() || pending_wake)
            && wake_authorized
            && stored.policy.wake_on_join
            && healthy
            && !blocked
            && snapshot.generation.is_none()
            && matches!(state, State::Sleeping | State::Waking)
        {
            self.wake(
                service,
                runtime,
                &subject,
                stored.policy.start_cooldown_seconds,
            )?;
        }
        if !stored.policy.enabled || !healthy || blocked || snapshot.phase != ProcessPhase::Running
        {
            self.blocker = if !stored.policy.enabled {
                SleepBlocker::Disabled
            } else if service.gate.blocked() {
                SleepBlocker::Hold
            } else if blocked {
                SleepBlocker::OperationInProgress
            } else {
                SleepBlocker::NotReady
            };
            self.reset();
            return Ok(());
        }
        let Some(generation) = snapshot.generation else {
            self.blocker = SleepBlocker::NotReady;
            self.reset();
            return Ok(());
        };
        if runtime
            .readiness()
            .map_err(|_| "sleep_state_unavailable")?
            .readiness
            != crate::operations::readiness::Readiness::Ready
        {
            self.blocker = SleepBlocker::NotReady;
            self.reset();
            return Ok(());
        }
        if service
            .db
            .read(|c| {
                authorize(
                    c,
                    &service.db,
                    &subject,
                    &[
                        Capability::LifecycleStop,
                        Capability::ConsoleRead,
                        Capability::ConsoleWrite,
                        Capability::PlayersView,
                    ],
                )
            })
            .is_err()
        {
            self.blocker = SleepBlocker::AuthorityUnavailable;
            self.reset();
            return Ok(());
        }
        if runtime.player_roster_revision(Some(generation)).is_some()
            && runtime
                .last_player_count(Some(generation))
                .is_some_and(|(count, _)| count > 0)
        {
            self.blocker = SleepBlocker::PlayersOnline;
            self.reset();
            return Ok(());
        }
        if let Some(cursor) = self.cursor {
            let page = runtime
                .console_snapshot(cursor, vesta_game_runtime::console::MAX_PAGE_LINES)
                .map_err(|_| "sleep_evidence_unavailable")?;
            let mut expected = cursor.saturating_add(1);
            let mut complete = true;
            for line in &page.lines {
                let own_warning = self
                    .warning_text
                    .as_ref()
                    .is_some_and(|warning| line.text.ends_with(warning));
                let empty_list = vesta_game_runtime::players::parse_player_list_line(&line.text)
                    .ok()
                    .is_some_and(|list| list.online() == 0);
                if line.cursor != expected || line.truncated || (!own_warning && !empty_list) {
                    complete = false;
                }
                expected = line.cursor.saturating_add(1);
            }
            if page.next_cursor != page.latest_cursor {
                complete = false;
            }
            if !complete {
                self.reset();
                self.blocker = SleepBlocker::PlayerCountUnknown;
            } else {
                self.cursor = Some(page.latest_cursor);
            }
        }
        if let Some(id) = &self.warning_id {
            let state = service
                .db
                .read(|c| {
                    c.query_row(
                        "SELECT state FROM operations WHERE server_id=? AND id=?",
                        params![service.db.server_id(), id],
                        |r| r.get::<_, String>(0),
                    )
                    .optional()
                    .map_err(Into::into)
                })
                .map_err(|_| "sleep_state_unavailable")?;
            match state.as_deref() {
                Some("succeeded") => {
                    self.warned_at.get_or_insert(now);
                }
                Some("queued" | "validating" | "running" | "cancelling") => {
                    self.blocker = SleepBlocker::OperationInProgress;
                    return Ok(());
                }
                _ => {
                    self.reset();
                    return Ok(());
                }
            }
        }
        self.update_countdown(&stored.policy, now);
        if self
            .sampled
            .is_some_and(|at| now.saturating_duration_since(at) < Duration::from_secs(10))
        {
            return Ok(());
        }
        self.sampled = Some(now);
        self.blocker = SleepBlocker::PlayerCountUnknown;
        // Two independent observations: a current status ping and a roster
        // seeded by list, then reconciled against every console activity line.
        let ping = vesta_game_runtime::readiness::status_probe(
            service.probe.ok_or("sleep_port_unknown")?,
            Duration::from_millis(750),
        );
        if !matches!(ping,Ok(summary) if summary.online==0) {
            self.blocker = if matches!(ping, Ok(summary) if summary.online > 0) {
                SleepBlocker::PlayersOnline
            } else {
                SleepBlocker::PlayerCountUnknown
            };
            self.reset();
            return Ok(());
        }
        let observation =
            match runtime.management_roster(&subject, generation, Duration::from_secs(2)) {
                Ok(o) => o,
                Err(_) => {
                    self.blocker = SleepBlocker::PlayerCountUnknown;
                    self.reset();
                    return Ok(());
                }
            };
        let tail = runtime
            .console_snapshot(observation.response_cursor(), 1)
            .map_err(|_| "sleep_evidence_unavailable")?;
        if !observation.players.is_empty()
            || tail.latest_cursor != observation.response_cursor()
            || runtime
                .game_snapshot()
                .map_err(|_| "sleep_state_unavailable")?
                .generation
                != Some(generation)
        {
            self.blocker = if observation.players.is_empty() {
                SleepBlocker::PlayerCountUnknown
            } else {
                SleepBlocker::PlayersOnline
            };
            self.reset();
            return Ok(());
        }
        self.cursor = Some(observation.response_cursor());
        let since = *self.zero_since.get_or_insert(now);
        self.update_countdown(&stored.policy, now);
        if now.saturating_duration_since(since)
            < Duration::from_secs(u64::from(stored.policy.idle_seconds))
        {
            return Ok(());
        }
        if self.warned_at.is_none() {
            let warning = format!(
                "Server will sleep in {} seconds unless a player joins.",
                stored.policy.warning_seconds
            );
            let admission = runtime
                .submit_game(
                    &subject,
                    GameCommand::Console {
                        expected_generation: generation,
                        line: format!("say {warning}"),
                    },
                    &format!(
                        "sleep-warning:{}:{revision}:{}",
                        generation.to_hex(),
                        observation.response_cursor()
                    ),
                )
                .map_err(|_| "sleep_warning_refused")?;
            let summary = match admission {
                crate::operations::Admission::Created(summary)
                | crate::operations::Admission::Existing(summary) => summary,
            };
            self.warning_id = Some(summary.id.as_str().into());
            self.warning_text = Some(warning);
            self.warned_at = None;
            self.blocker = SleepBlocker::OperationInProgress;
            return Ok(());
        }
        if self.warned_at.is_some_and(|at| {
            now.saturating_duration_since(at)
                < Duration::from_secs(u64::from(stored.policy.warning_seconds))
        }) {
            return Ok(());
        }
        let key = format!(
            "sleep-stop:{}:{}",
            generation.to_hex(),
            observation.response_cursor()
        );
        // Record the exact intent before admission, so restart cannot infer a
        // successful sleep from a stopped process or generate replacement input.
        service
            .db
            .write(|c| {
                let mut record = load_record(c, &service.db)?;
                record.sleep_key = Some(key.clone());
                put(c, &service.db, RUNTIME_KEY, &record)
            })
            .map_err(|_| "sleep_state_unavailable")?;
        runtime
            .submit_game(
                &subject,
                GameCommand::Sleep {
                    expected_generation: generation,
                    expected_cursor: observation.response_cursor(),
                },
                &key,
            )
            .map_err(|_| "sleep_stop_refused")?;
        self.reset();
        self.blocker = SleepBlocker::OperationInProgress;
        Ok(())
    }
    fn update_countdown(&mut self, policy: &Policy, now: Instant) {
        let remaining = |since, seconds| {
            Duration::from_secs(u64::from(seconds))
                .saturating_sub(now.saturating_duration_since(since))
                .as_secs_f64()
                .ceil() as u32
        };
        if let Some(at) = self.warned_at {
            self.blocker = SleepBlocker::WarningSent {
                remaining_seconds: remaining(at, policy.warning_seconds),
            };
        } else if let Some(at) = self.zero_since {
            self.blocker = SleepBlocker::CountingDown {
                remaining_seconds: remaining(at, policy.idle_seconds),
            };
        }
    }
    fn reconcile(
        &self,
        service: &Sleep,
        runtime: &RuntimeHandle,
        subject: &Subject,
        generation: Option<GenerationId>,
    ) -> Result<(), &'static str> {
        let mut record = service
            .db
            .read(|c| load_record(c, &service.db))
            .map_err(|_| "sleep_state_unavailable")?;
        let mut changed = false;
        if generation.is_some() && (record.sleeping || record.wake_key.is_some()) {
            record.sleeping = false;
            record.wake_key = None;
            changed = true;
        }
        if let Some(key) = &record.sleep_key
            && let Some(operation) = runtime
                .operations_service()
                .find_key_trusted(&subject.principal, key)
                .map_err(|_| "sleep_state_unavailable")?
            && operation.state.is_terminal()
        {
            record.sleeping = operation.state == OperationState::Succeeded && generation.is_none();
            record.sleep_key = None;
            changed = true;
        }
        if let Some(key) = &record.wake_key
            && let Some(operation) = runtime
                .operations_service()
                .find_key_trusted(&subject.principal, key)
                .map_err(|_| "sleep_state_unavailable")?
            && operation.state.is_terminal()
        {
            record.wake_key = None;
            changed = true;
        }
        if changed {
            service
                .db
                .write(|c| put(c, &service.db, RUNTIME_KEY, &record))
                .map_err(|_| "sleep_state_unavailable")?;
        }
        Ok(())
    }
    fn wake(
        &self,
        service: &Sleep,
        runtime: &RuntimeHandle,
        subject: &Subject,
        cooldown: u32,
    ) -> Result<(), &'static str> {
        let now = service.db.clock().millis();
        let key = service
            .db
            .write(|c| {
                authorize(c, &service.db, subject, &[Capability::LifecycleStart])?;
                let mut record = load_record(c, &service.db)?;
                if let Some(key) = record.wake_key {
                    return Ok(Some(key));
                }
                if now < record.cooldown_until_ms || blocked_in(c, service.db.server_id(), None)? {
                    return Ok(None);
                }
                record.sequence = record
                    .sequence
                    .checked_add(1)
                    .ok_or_else(|| StateError::new("state_sleep_state_invalid"))?;
                let key = format!("sleep-wake:{}", record.sequence);
                record.wake_key = Some(key.clone());
                record.cooldown_until_ms = now.saturating_add(i64::from(cooldown) * 1000);
                put(c, &service.db, RUNTIME_KEY, &record)?;
                Ok(Some(key))
            })
            .map_err(|_| "sleep_wake_refused")?;
        if let Some(key) = key {
            runtime
                .submit_game(subject, GameCommand::Start, &key)
                .map_err(|_| "sleep_wake_refused")?;
        }
        Ok(())
    }
}

/// Persist only proven lifecycle outcomes, in the operation completion transaction.
pub(crate) fn lifecycle_finished_in(
    c: &Connection,
    server: &str,
    now: i64,
    kind: &str,
) -> vesta_state::Result<()> {
    let sleeping = match kind {
        "game.sleep" | "game.manual_sleep" => true,
        "game.stop" | "game.stop_sleeping" | "game.start" | "game.restart" | "game.force_kill" => {
            false
        }
        _ => return Ok(()),
    };
    let raw: Option<String> = c
        .query_row(
            "SELECT desired_value FROM settings WHERE server_id=? AND key=?",
            params![server, RUNTIME_KEY],
            |r| r.get(0),
        )
        .optional()?;
    let mut record: Record = raw
        .map(|v| serde_json::from_str(&v))
        .transpose()
        .map_err(|_| StateError::new("state_sleep_state_invalid"))?
        .unwrap_or_default();
    record.sleeping = sleeping;
    record.sleep_key = None;
    record.wake_key = None;
    let value =
        serde_json::to_string(&record).map_err(|_| StateError::new("state_sleep_state_invalid"))?;
    c.execute("INSERT INTO settings(server_id,key,desired_value,desired_revision,apply_mode,effective_value,effective_revision,updated_at) VALUES(?1,?2,?3,1,'immediate',?3,1,?4) ON CONFLICT(server_id,key) DO UPDATE SET desired_value=excluded.desired_value,desired_revision=settings.desired_revision+1,effective_value=excluded.desired_value,effective_revision=settings.desired_revision+1,updated_at=excluded.updated_at",params![server,RUNTIME_KEY,value,now])?;
    Ok(())
}
