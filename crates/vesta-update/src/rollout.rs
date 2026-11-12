//! Conservative rollout previews. Caller facts are observations, never authority.
//! No result from this module executes processes or authorises activation.
use serde::Serialize;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GameState {
    Stopped,
    Running,
    Unknown,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ownership {
    SeparateSupervisor,
    InProcess,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Compatibility {
    Compatible,
    Incompatible,
    Unknown,
}

/// Installation-scoped native observations must be refreshed before a real
/// executor can act. Do not accept these fields as remote client assertions.
#[derive(Clone, Debug)]
pub struct ConsumerObservation {
    pub server_id: String,
    pub game: GameState,
    pub ownership: Ownership,
    pub generation: Option<String>,
    pub unresolved_launch: bool,
    pub pending_writes: bool,
    pub restart_pending: bool,
    pub agent_current: bool,
    pub supervisor_current: bool,
    pub ipc: Compatibility,
    pub state_schema: Compatibility,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ConsumerAction {
    NoChange,
    RestartStoppedServices,
    HandoffAgentRetainSupervisor,
    RetainServicesUntilStopped,
    MaintenanceRequired,
    Deferred,
    Blocked,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Reason {
    AlreadyCurrent,
    Stopped,
    CompatibleLiveSupervisor,
    SupervisorBusy,
    InProcessGameRunning,
    IpcIncompatible,
    IpcUnproven,
    SchemaMigrationRequired,
    SchemaUnproven,
    ProcessUnobservable,
    GenerationUnproven,
    LaunchUnresolved,
    WritesPending,
    RestartPending,
}
#[derive(Debug, Serialize)]
pub struct ConsumerPreview {
    pub server_id: String,
    pub action: ConsumerAction,
    pub reason: Reason,
    pub supervisor_update_pending: bool,
    pub preserves_game_generation: bool,
}
#[derive(Debug, Serialize)]
pub struct RolloutPreview {
    pub consumers: Vec<ConsumerPreview>,
    pub requires_maintenance: bool,
    pub has_deferred_or_blocked: bool,
    /// Executable replacement is deliberately not implemented by a preview.
    pub activation_supported: bool,
}

/// Planning is independent of Minecraft/loader/pack updates. Even an empty
/// registry is not proof that installation-wide activation is safe.
pub fn preview_rollout(consumers: &[ConsumerObservation]) -> RolloutPreview {
    let rows: Vec<_> = consumers.iter().map(preview_consumer).collect();
    RolloutPreview {
        requires_maintenance: rows
            .iter()
            .any(|c| c.action == ConsumerAction::MaintenanceRequired),
        has_deferred_or_blocked: rows
            .iter()
            .any(|c| matches!(c.action, ConsumerAction::Deferred | ConsumerAction::Blocked)),
        consumers: rows,
        activation_supported: false,
    }
}

fn preview_consumer(c: &ConsumerObservation) -> ConsumerPreview {
    use ConsumerAction::*;
    use Reason::*;
    let (action, reason) = if c.unresolved_launch {
        (Blocked, LaunchUnresolved)
    } else if c.game == GameState::Unknown {
        (Blocked, ProcessUnobservable)
    } else if c.game == GameState::Running && c.generation.as_ref().is_none_or(|g| g.is_empty()) {
        (Blocked, GenerationUnproven)
    } else if c.restart_pending {
        (Deferred, RestartPending)
    } else if c.pending_writes {
        (Deferred, WritesPending)
    } else if c.agent_current && (c.ownership == Ownership::InProcess || c.supervisor_current) {
        (NoChange, AlreadyCurrent)
    } else if c.state_schema == Compatibility::Unknown {
        (Blocked, SchemaUnproven)
    } else if c.state_schema == Compatibility::Incompatible {
        (MaintenanceRequired, SchemaMigrationRequired)
    } else if c.game == GameState::Stopped {
        (RestartStoppedServices, Stopped)
    } else if c.ownership == Ownership::InProcess {
        (MaintenanceRequired, InProcessGameRunning)
    } else if c.ipc == Compatibility::Unknown {
        (Blocked, IpcUnproven)
    } else if c.ipc == Compatibility::Incompatible {
        (MaintenanceRequired, IpcIncompatible)
    } else if c.agent_current {
        (RetainServicesUntilStopped, SupervisorBusy)
    } else {
        (HandoffAgentRetainSupervisor, CompatibleLiveSupervisor)
    };
    ConsumerPreview {
        server_id: c.server_id.clone(),
        action,
        reason,
        supervisor_update_pending: c.ownership == Ownership::SeparateSupervisor
            && !c.supervisor_current
            && c.game != GameState::Stopped,
        preserves_game_generation: matches!(
            action,
            HandoffAgentRetainSupervisor | RetainServicesUntilStopped
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn running() -> ConsumerObservation {
        ConsumerObservation {
            server_id: "fixture-server".into(),
            game: GameState::Running,
            ownership: Ownership::SeparateSupervisor,
            generation: Some("fixture-generation".into()),
            unresolved_launch: false,
            pending_writes: false,
            restart_pending: false,
            agent_current: false,
            supervisor_current: false,
            ipc: Compatibility::Compatible,
            state_schema: Compatibility::Compatible,
        }
    }
    #[test]
    fn live_compatible_preview_retains_old_supervisor_and_exact_generation() {
        let c = running();
        let p = preview_rollout(std::slice::from_ref(&c));
        let r = &p.consumers[0];
        assert_eq!(r.action, ConsumerAction::HandoffAgentRetainSupervisor);
        assert!(r.supervisor_update_pending);
        assert!(r.preserves_game_generation);
        assert!(!p.activation_supported);
        assert!(!p.requires_maintenance);
        assert_eq!(c.generation.as_deref(), Some("fixture-generation"));
    }
    #[test]
    fn live_in_process_or_incompatible_wire_needs_maintenance() {
        let mut c = running();
        c.ownership = Ownership::InProcess;
        assert_eq!(preview_consumer(&c).reason, Reason::InProcessGameRunning);
        c.ownership = Ownership::SeparateSupervisor;
        c.ipc = Compatibility::Incompatible;
        let p = preview_rollout(&[c]);
        assert!(p.requires_maintenance);
        assert_eq!(p.consumers[0].reason, Reason::IpcIncompatible);
    }
    #[test]
    fn absent_compatibility_or_process_evidence_does_not_grant_handoff() {
        let mut c = running();
        c.ipc = Compatibility::Unknown;
        assert_eq!(preview_consumer(&c).reason, Reason::IpcUnproven);
        c.ipc = Compatibility::Compatible;
        c.state_schema = Compatibility::Unknown;
        assert_eq!(preview_consumer(&c).reason, Reason::SchemaUnproven);
        c.state_schema = Compatibility::Compatible;
        c.game = GameState::Unknown;
        assert_eq!(preview_consumer(&c).reason, Reason::ProcessUnobservable);
        c.game = GameState::Running;
        c.generation = None;
        assert_eq!(preview_consumer(&c).reason, Reason::GenerationUnproven);
    }
    #[test]
    fn writes_restarts_and_launch_uncertainty_fence_even_current_payloads() {
        let mut c = running();
        c.agent_current = true;
        c.supervisor_current = true;
        c.pending_writes = true;
        assert_eq!(preview_consumer(&c).reason, Reason::WritesPending);
        c.restart_pending = true;
        assert_eq!(preview_consumer(&c).reason, Reason::RestartPending);
        c.unresolved_launch = true;
        assert_eq!(preview_consumer(&c).action, ConsumerAction::Blocked);
    }
    #[test]
    fn stopped_does_not_bypass_schema_migration_or_uncertainty() {
        let mut c = running();
        c.game = GameState::Stopped;
        c.generation = None;
        c.ipc = Compatibility::Incompatible;
        assert_eq!(
            preview_consumer(&c).action,
            ConsumerAction::RestartStoppedServices
        );
        c.state_schema = Compatibility::Incompatible;
        assert_eq!(preview_consumer(&c).reason, Reason::SchemaMigrationRequired);
        c.state_schema = Compatibility::Unknown;
        assert_eq!(preview_consumer(&c).action, ConsumerAction::Blocked);
    }
    #[test]
    fn supervisor_only_update_is_pending_until_game_stops() {
        let mut c = running();
        c.agent_current = true;
        let row = preview_consumer(&c);
        assert_eq!(row.action, ConsumerAction::RetainServicesUntilStopped);
        assert!(row.supervisor_update_pending);
        c.game = GameState::Stopped;
        let row = preview_consumer(&c);
        assert_eq!(row.action, ConsumerAction::RestartStoppedServices);
        assert!(!row.supervisor_update_pending);
    }
    #[test]
    fn mixed_consumers_and_empty_registry_cannot_claim_activation() {
        let a = running();
        let mut b = running();
        b.server_id = "other".into();
        b.pending_writes = true;
        let p = preview_rollout(&[a, b]);
        assert!(p.has_deferred_or_blocked);
        assert_eq!(p.consumers.len(), 2);
        assert!(!p.activation_supported);
        assert!(!preview_rollout(&[]).activation_supported);
    }
}
