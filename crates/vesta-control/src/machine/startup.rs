//! Startup settings belong to the Host; reports belong to the worker that ran them.
use super::{
    HostState,
    owners::Actor,
    registry::require_server,
    state::{changed, expect_revision, reject},
};
use crate::{IdentityErrorCode as Code, Result, config::OnAgentStart, identity::Capability};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StartupAction {
    #[default]
    RestorePrevious,
    StayStopped,
    Sleep,
    Start,
}

impl StartupAction {
    pub(crate) fn explicit(self) -> Option<OnAgentStart> {
        match self {
            Self::RestorePrevious => None,
            Self::StayStopped => Some(OnAgentStart::StayStopped),
            Self::Sleep => Some(OnAgentStart::Sleep),
            Self::Start => Some(OnAgentStart::Start),
        }
    }
}

impl From<Option<OnAgentStart>> for StartupAction {
    fn from(action: Option<OnAgentStart>) -> Self {
        match action {
            None => Self::RestorePrevious,
            Some(OnAgentStart::StayStopped) => Self::StayStopped,
            Some(OnAgentStart::Sleep) => Self::Sleep,
            Some(OnAgentStart::Start) => Self::Start,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
pub struct StartupSettings {
    pub on_agent_start: StartupAction,
    pub start_delay_seconds: u32,
}

#[derive(Clone, Copy, Debug, Default)]
pub enum StartupMode {
    #[default]
    Configured,
    Deferred,
    Managed(StartupSettings),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StartupReport {
    pub at_ms: i64,
    pub applied_action: OnAgentStart,
    pub outcome: StartupOutcome,
    pub error_code: Option<String>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StartupOutcome {
    Pending,
    Applied,
    AlreadyRunning,
    Failed,
}

fn read_in(c: &Connection, server: Uuid) -> Result<Option<StartupSettings>> {
    let value: Option<String> = c
        .query_row(
            "SELECT document FROM machine_startup WHERE server_id=?",
            [server.to_string()],
            |r| r.get(0),
        )
        .optional()?;
    value
        .map(|value| serde_json::from_str(&value).map_err(|_| reject(Code::InvalidRequest)))
        .transpose()
}

fn save_in(c: &Connection, server: Uuid, settings: StartupSettings) -> Result<()> {
    let value = serde_json::to_string(&settings).map_err(|_| reject(Code::InvalidRequest))?;
    c.execute("INSERT INTO machine_startup VALUES(?,?) ON CONFLICT(server_id) DO UPDATE SET document=excluded.document",
        params![server.to_string(), value])?;
    Ok(())
}

pub(super) fn register_in(c: &Connection, server: Uuid) -> Result<()> {
    save_in(
        c,
        server,
        StartupSettings {
            on_agent_start: super::settings::startup_default_in(c)?,
            start_delay_seconds: 0,
        },
    )
}

impl HostState {
    pub fn startup_settings(&self, server: Uuid) -> Result<Option<StartupSettings>> {
        self.read(|c| read_in(c, server))
    }

    pub fn startup_open_failure(&self, server: Uuid) -> Result<Option<StartupReport>> {
        self.read(|c| {
            let value: Option<String> = c
                .query_row(
                    "SELECT document FROM machine_startup_failures WHERE server_id=?",
                    [server.to_string()],
                    |row| row.get(0),
                )
                .optional()?;
            value
                .map(|value| serde_json::from_str(&value).map_err(|_| reject(Code::InvalidRequest)))
                .transpose()
        })
    }

    pub fn record_startup_open_failure(&self, server: Uuid, code: &'static str) -> Result<()> {
        self.write(|c| {
            require_server(c, server)?;
            let report = StartupReport {
                at_ms: self.now_ms(),
                applied_action: read_in(c, server)?.and_then(|s| s.on_agent_start.explicit()).unwrap_or_default(),
                outcome: StartupOutcome::Failed,
                error_code: Some(code.into()),
            };
            let document = serde_json::to_string(&report).map_err(|_| reject(Code::InvalidRequest))?;
            c.execute("INSERT INTO machine_startup_failures VALUES(?,?) ON CONFLICT(server_id) DO UPDATE SET document=excluded.document",
                params![server.to_string(), document])?;
            Ok(())
        })
    }

    pub fn patch_startup(
        &self,
        actor: Actor,
        server: Uuid,
        revision: u64,
        fallback: StartupSettings,
        action: Option<StartupAction>,
        delay: Option<u32>,
    ) -> Result<(u64, StartupSettings)> {
        if delay.is_some_and(|delay| delay > 3600) || action.is_none() && delay.is_none() {
            return Err(reject(Code::InvalidRequest));
        }
        self.write(|c| {
            require_server(c, server)?;
            let mut settings = read_in(c, server)?.unwrap_or(fallback);
            if let Some(action) = action {
                settings.on_agent_start = action;
            }
            super::roles::require_capability(c, actor, server, Capability::ConfigEdit)?;
            let lifecycle = match settings.on_agent_start {
                StartupAction::RestorePrevious | StartupAction::Start => Capability::LifecycleStart,
                StartupAction::StayStopped | StartupAction::Sleep => Capability::LifecycleStop,
            };
            super::roles::require_capability(c, actor, server, lifecycle)?;
            expect_revision(c, revision)?;
            if let Some(delay) = delay {
                settings.start_delay_seconds = delay;
            }
            save_in(c, server, settings)?;
            let snapshot = changed(
                c,
                false,
                "server_startup_settings",
                &actor.audit(),
                &server.to_string(),
                self.now_ms(),
            )?;
            Ok((snapshot.revision, settings))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::machine::{
        registry::{ServerEntry, ServerState, Visibility},
        settings::HostSettingsPatch,
    };
    use std::sync::Arc;

    #[test]
    fn startup_defaults_are_copied_and_server_edits_are_versioned() {
        let tmp = tempfile::tempdir().unwrap();
        let state =
            HostState::open(tmp.path().join("host"), Arc::new(vesta_state::SystemClock)).unwrap();
        let owner = state.claim(Uuid::new_v4(), "Owner", [5; 32]).unwrap();
        let actor = Actor::Account(owner.account_id);
        let register = |port| {
            let server_id = Uuid::new_v4();
            state
                .register_server(
                    actor,
                    &ServerEntry {
                        server_id,
                        name: format!("Server {port}"),
                        game_port: port,
                        state: ServerState::Stopped,
                        visibility: Visibility::Private,
                    },
                    state.snapshot().unwrap().revision,
                )
                .unwrap();
            server_id
        };
        let first = register(31011);
        state
            .update_settings(
                actor,
                HostSettingsPatch {
                    name: None,
                    new_device_sign_in: None,
                    removed_server_retention_days: None,
                    default_on_agent_start: Some(StartupAction::Start),
                },
            )
            .unwrap();
        let second = register(31012);
        assert_eq!(
            state
                .startup_settings(first)
                .unwrap()
                .unwrap()
                .on_agent_start,
            StartupAction::RestorePrevious
        );
        assert_eq!(
            state
                .startup_settings(second)
                .unwrap()
                .unwrap()
                .on_agent_start,
            StartupAction::Start
        );
        let revision = state.snapshot().unwrap().revision;
        let (_, policy) = state
            .patch_startup(actor, second, revision, Default::default(), None, Some(25))
            .unwrap();
        assert_eq!(policy.start_delay_seconds, 25);
        assert_eq!(
            state
                .patch_startup(actor, second, revision, Default::default(), None, Some(10))
                .unwrap_err()
                .code(),
            "conflict"
        );
        state
            .record_startup_open_failure(second, "worker_unavailable")
            .unwrap();
        let report = state.startup_open_failure(second).unwrap().unwrap();
        assert!(matches!(report.outcome, StartupOutcome::Failed));
        assert_eq!(report.error_code.as_deref(), Some("worker_unavailable"));
    }
}
