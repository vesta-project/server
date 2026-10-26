//! Strict, bounded definitions for the local sequential automation engine.
use super::cron::Cron;
use crate::{ControlError, IdentityErrorCode};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

// Serde ignores unknown fields on internally tagged unit variants, even with
// deny_unknown_fields. Empty struct wire variants preserve a strict contract.
macro_rules! strict_tagged_enum {
    ($(#[$attrs:meta])* pub enum $name:ident {
        $($(#[$variant_attrs:meta])* $variant:ident $( { $($field:ident: $ty:ty),* $(,)? } )?),* $(,)?
    }) => {
        $(#[$attrs])*
        #[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
        pub enum $name {
            $($(#[$variant_attrs])* $variant $( { $($field: $ty),* } )?),*
        }
        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
                #[derive(Deserialize)]
                #[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
                enum Wire {
                    $($variant { $($($field: $ty),*)? }),*
                }
                Ok(match Wire::deserialize(deserializer)? {
                    $(Wire::$variant { $($($field),*)? } => Self::$variant $( { $($field),* } )?),*
                })
            }
        }
    };
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkflowDefinition {
    pub trigger: Trigger,
    #[serde(default)]
    pub missed_policy: MissedPolicy,
    pub steps: Vec<Step>,
}

strict_tagged_enum! {
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub enum Trigger {
    Manual,
    Cron {
        expr: String,
        utc_offset_minutes: i16,
    },
    Interval {
        seconds: u32,
    },
    AgentStartup,
    ServerStarted,
    ServerStopped,
    ServerCrashed,
    PlayerCountZero,
    FirstPlayerJoined,
    PlayerFirstJoined,
    BackupCompleted {
        profile: Option<String>,
    },
    BackupFailed {
        profile: Option<String>,
    },
    UpdateAvailable,
}
}

strict_tagged_enum! {
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
pub enum MissedPolicy {
    #[default]
    SkipAndNotify,
    RunLatest,
    CatchUp {
        max: u8,
    },
}
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Step {
    pub id: String,
    pub action: Action,
    pub timeout_secs: u32,
    #[serde(default)]
    pub retries: RetryPolicy,
    #[serde(default)]
    pub on_success: SuccessBranch,
    #[serde(default)]
    pub on_failure: FailureBranch,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RetryPolicy {
    pub max_retries: u8,
    pub backoff_secs: u32,
}

strict_tagged_enum! {
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub enum SuccessBranch {
    #[default]
    Next,
    Goto {
        step_id: String,
    },
    End,
    IfOutputEquals {
        key: String,
        value: Value,
        then_step: String,
        otherwise: Option<String>,
    },
}
}

strict_tagged_enum! {
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
pub enum FailureBranch {
    #[default]
    Fail,
    Next,
    Goto {
        step_id: String,
    },
}
}

strict_tagged_enum! {
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub enum Action {
    Delay {
        secs: u32,
    },
    Backup {
        profile: String,
    },
    VerifyBackup {
        profile: String,
    },
    Start,
    Stop,
    Restart,
    ConsoleCommand {
        line: String,
    },
    CleanTrash,
    Notify {
        severity: Severity,
        title: String,
        body: String,
    },
    ScriptPreset {
        preset: String,
        params: BTreeMap<String, String>,
    },
}
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum Severity {
    Info,
    Warning,
    Critical,
}
impl Severity {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Info => "info",
            Self::Warning => "warning",
            Self::Critical => "critical",
        }
    }
}

strict_tagged_enum! {
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub enum AutomationEvent {
    AgentStarted,
    ServerStarted,
    ServerStopped,
    ServerCrashed,
    PlayerCountZero,
    FirstPlayerJoined,
    PlayerFirstJoined { player: String, uuid: String },
    BackupCompleted { profile: String },
    BackupFailed { profile: String },
    UpdateAvailable,
}
}
impl AutomationEvent {
    pub fn matches_trigger(&self, trigger: &Trigger) -> bool {
        match (self, trigger) {
            (Self::PlayerFirstJoined { player, uuid }, Trigger::PlayerFirstJoined) => crate::moderation::valid_player(player) && uuid::Uuid::parse_str(uuid).is_ok(),
            (Self::AgentStarted, Trigger::AgentStartup)
            | (Self::ServerStarted, Trigger::ServerStarted)
            | (Self::ServerStopped, Trigger::ServerStopped)
            | (Self::ServerCrashed, Trigger::ServerCrashed)
            | (Self::PlayerCountZero, Trigger::PlayerCountZero)
            | (Self::FirstPlayerJoined, Trigger::FirstPlayerJoined)
            | (Self::UpdateAvailable, Trigger::UpdateAvailable) => true,
            (Self::BackupCompleted { profile }, Trigger::BackupCompleted { profile: filter })
            | (Self::BackupFailed { profile }, Trigger::BackupFailed { profile: filter }) => {
                bounded_text(profile, 128, false) && filter.as_ref().is_none_or(|f| f == profile)
            }
            _ => false,
        }
    }
}

fn invalid() -> ControlError {
    ControlError::rejected(IdentityErrorCode::InvalidRequest)
}

/// UTF-8 byte length bound, with no control characters. Whitespace-only text is
/// not treated as a meaningful value when empty values are disallowed.
pub fn bounded_text(value: &str, max: usize, allow_empty: bool) -> bool {
    value.len() <= max
        && (allow_empty || !value.trim().is_empty())
        && !value.chars().any(char::is_control)
}

/// Output bounds apply before serialization: at most eight nested containers,
/// 1,024 total values, 256 elements per array, 128 properties per object,
/// 4,096 bytes per string and 128 bytes per key. The encoded result is <=16KiB.
pub fn bounded_output(value: &Value) -> bool {
    fn walk(value: &Value, depth: usize, budget: &mut usize) -> bool {
        if *budget == 0 {
            return false;
        }
        *budget -= 1;
        match value {
            Value::Array(values) => {
                depth < 8
                    && values.len() <= 256
                    && values.iter().all(|v| walk(v, depth + 1, budget))
            }
            Value::Object(values) => {
                depth < 8
                    && values.len() <= 128
                    && values
                        .iter()
                        .all(|(k, v)| bounded_text(k, 128, false) && walk(v, depth + 1, budget))
            }
            Value::String(s) => s.len() <= 4096,
            _ => true,
        }
    }
    struct Counter(usize);
    impl std::io::Write for Counter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if bytes.len() > 16_384 - self.0 {
                return Err(std::io::Error::other("output limit"));
            }
            self.0 += bytes.len();
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    walk(value, 0, &mut 1024) && serde_json::to_writer(Counter(0), value).is_ok()
}

impl Action {
    /// Only delay and notification bypass the engine's mutating-authority check.
    pub const fn mutates(&self) -> bool {
        !matches!(self, Self::Delay { .. } | Self::Notify { .. })
    }
    pub fn validate(&self) -> crate::Result<()> {
        let valid = match self {
            Self::Delay { secs } => (1..=86_400).contains(secs),
            Self::Backup { profile } | Self::VerifyBackup { profile } => {
                bounded_text(profile, 128, false)
            }
            Self::ConsoleCommand { line } => bounded_text(line, 1024, false),
            Self::Notify { title, body, .. } => {
                bounded_text(title, 256, false) && bounded_text(body, 4096, true)
            }
            Self::ScriptPreset { preset, params } => {
                bounded_text(preset, 128, false)
                    && params.len() <= 32
                    && params.iter().all(|(key, value)| {
                        bounded_text(key, 64, false) && bounded_text(value, 2048, true)
                    })
                    && bounded_output(&serde_json::to_value(params).map_err(|_| invalid())?)
            }
            Self::Start | Self::Stop | Self::Restart | Self::CleanTrash => true,
        };
        if valid { Ok(()) } else { Err(invalid()) }
    }
}

impl WorkflowDefinition {
    /// Bounded wire entry point. Direct serde deserialization checks field shape;
    /// services always call `validate` before accepting in-memory definitions.
    pub fn from_json(bytes: &[u8]) -> crate::Result<Self> {
        if bytes.len() > 65_536 {
            return Err(invalid());
        }
        let value: Self = serde_json::from_slice(bytes).map_err(|_| invalid())?;
        value.validate()?;
        Ok(value)
    }
    pub fn validate(&self) -> crate::Result<()> {
        match &self.trigger {
            Trigger::Cron {
                expr,
                utc_offset_minutes,
            } => {
                if !(-840..=840).contains(utc_offset_minutes) {
                    return Err(invalid());
                }
                Cron::parse(expr)?;
            }
            Trigger::Interval { seconds } if !(60..=31_536_000).contains(seconds) => {
                return Err(invalid());
            }
            Trigger::BackupCompleted {
                profile: Some(profile),
            }
            | Trigger::BackupFailed {
                profile: Some(profile),
            } if !bounded_text(profile, 128, false) => return Err(invalid()),
            _ => {}
        }
        if let MissedPolicy::CatchUp { max } = self.missed_policy
            && !(1..=10).contains(&max)
        {
            return Err(invalid());
        }
        if self.steps.is_empty() || self.steps.len() > 32 {
            return Err(invalid());
        }
        let mut ids = BTreeSet::new();
        for step in &self.steps {
            if !bounded_text(&step.id, 128, false)
                || !ids.insert(step.id.as_str())
                || !(1..=86_400).contains(&step.timeout_secs)
                || step.retries.max_retries > 5
                || step.retries.backoff_secs > 86_400
            {
                return Err(invalid());
            }
            step.action.validate()?;
        }
        // All explicit branches must point forward. This excludes both cycles
        // and reexecution of a previous non-idempotent step.
        for (index, step) in self.steps.iter().enumerate() {
            let forward = |id: &str| self.steps[index + 1..].iter().any(|s| s.id == id);
            let success_valid = match &step.on_success {
                SuccessBranch::Goto { step_id } => forward(step_id),
                SuccessBranch::IfOutputEquals {
                    key,
                    value,
                    then_step,
                    otherwise,
                } => {
                    bounded_text(key, 128, false)
                        && bounded_output(value)
                        && forward(then_step)
                        && otherwise.as_ref().is_none_or(|id| forward(id))
                }
                _ => true,
            };
            let failure_valid = match &step.on_failure {
                FailureBranch::Goto { step_id } => forward(step_id),
                _ => true,
            };
            if !success_valid || !failure_valid {
                return Err(invalid());
            }
        }
        if serde_json::to_vec(self).map_err(|_| invalid())?.len() > 65_536 {
            return Err(invalid());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn step(id: &str) -> Step {
        Step {
            id: id.into(),
            action: Action::Start,
            timeout_secs: 60,
            retries: RetryPolicy::default(),
            on_success: SuccessBranch::Next,
            on_failure: FailureBranch::Fail,
        }
    }
    fn workflow() -> WorkflowDefinition {
        WorkflowDefinition {
            trigger: Trigger::Manual,
            missed_policy: MissedPolicy::default(),
            steps: vec![step("one"), step("two")],
        }
    }
    #[test]
    fn bounds_and_forward_branches() {
        let mut w = workflow();
        w.steps[0].on_success = SuccessBranch::Goto {
            step_id: "two".into(),
        };
        assert!(w.validate().is_ok());
        w.steps[1].on_failure = FailureBranch::Goto {
            step_id: "one".into(),
        };
        assert!(w.validate().is_err());
        w.steps[1].on_failure = FailureBranch::Fail;
        w.steps[0].retries.max_retries = 6;
        assert!(w.validate().is_err());
        w.steps[0].retries.max_retries = 5;
        w.steps[1].id = "one".into();
        assert!(w.validate().is_err());
        w.steps = (0..33).map(|i| step(&i.to_string())).collect();
        assert!(w.validate().is_err());
        w.steps.clear();
        assert!(w.validate().is_err());
    }
    #[test]
    fn strict_serde_and_defaults() {
        let w: WorkflowDefinition = serde_json::from_value(serde_json::json!({
            "trigger":{"kind":"manual"}, "steps":[{"id":"start","action":{"kind":"start"},"timeout_secs":60}]
        })).unwrap();
        assert_eq!(w.missed_policy, MissedPolicy::SkipAndNotify);
        assert_eq!(w.steps[0].retries, RetryPolicy::default());
        for value in [
            serde_json::json!({"kind":"start","shell":"bad"}),
            serde_json::json!({"kind":"delay","secs":1,"extra":true}),
        ] {
            assert!(serde_json::from_value::<Action>(value).is_err());
        }
        assert!(
            serde_json::from_value::<Trigger>(serde_json::json!({"kind":"manual","extra":true}))
                .is_err()
        );
        assert!(
            serde_json::from_value::<RetryPolicy>(
                serde_json::json!({"max_retries":0,"extra":true})
            )
            .is_err()
        );
    }
    #[test]
    fn output_depth_size_and_text_limits() {
        let mut v = Value::Null;
        for _ in 0..8 {
            v = serde_json::json!([v]);
        }
        assert!(bounded_output(&v));
        v = serde_json::json!([v]);
        assert!(!bounded_output(&v));
        assert!(!bounded_output(&serde_json::json!(vec![0; 257])));
        assert!(!bounded_output(&serde_json::json!("x".repeat(4097))));
        assert!(!bounded_output(&serde_json::json!(vec![
            "x".repeat(4096);
            4
        ])));
        assert!(bounded_text("", 1, true));
        assert!(!bounded_text(" ", 1, false));
        assert!(!bounded_text("a\nb", 10, true));
    }
    #[test]
    fn action_and_trigger_validation() {
        assert!(!Action::Delay { secs: 1 }.mutates());
        assert!(
            !Action::Notify {
                severity: Severity::Info,
                title: "t".into(),
                body: "".into()
            }
            .mutates()
        );
        assert!(
            Action::VerifyBackup {
                profile: "p".into()
            }
            .mutates()
        );
        assert!(
            Action::ConsoleCommand {
                line: "say hi\nstop".into()
            }
            .validate()
            .is_err()
        );
        let mut w = workflow();
        w.trigger = Trigger::Interval { seconds: 59 };
        assert!(w.validate().is_err());
        w.trigger = Trigger::Cron {
            expr: "* * * * *".into(),
            utc_offset_minutes: 841,
        };
        assert!(w.validate().is_err());
        w.trigger = Trigger::Manual;
        w.steps[0].on_success = SuccessBranch::IfOutputEquals {
            key: "result".into(),
            value: Value::Bool(true),
            then_step: "two".into(),
            otherwise: Some("one".into()),
        };
        assert!(w.validate().is_err());
    }
    #[test]
    fn typed_events_match_only_eligible_triggers() {
        let e = AutomationEvent::BackupCompleted {
            profile: "daily".into(),
        };
        assert!(e.matches_trigger(&Trigger::BackupCompleted { profile: None }));
        assert!(e.matches_trigger(&Trigger::BackupCompleted {
            profile: Some("daily".into())
        }));
        assert!(!e.matches_trigger(&Trigger::BackupCompleted {
            profile: Some("weekly".into())
        }));
        assert!(!e.matches_trigger(&Trigger::BackupFailed { profile: None }));
        assert!(!e.matches_trigger(&Trigger::Manual));
        assert_eq!(Severity::Critical.as_str(), "critical");
    }
}
