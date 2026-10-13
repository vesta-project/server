//! Closed, pure game-lifecycle bindings. Input selects a requested action, never its authority.
use super::{
    domain::{OperationsResult, invalid},
    input::{PreparedOperation, Requirement, digest},
};
use crate::{identity::Capability, moderation::ModerationAction};
use serde::{Deserialize, Serialize};
use vesta_game_runtime::process::GenerationId;

pub(super) const GAME_LOCK: &str = "game.generation";

/// A generation is an opaque lookup key, not a PID, authenticated identity or authority.
/// Launch paths, arguments, capability/lock/safety declarations and revisions are server-owned.
#[derive(Clone, Eq, PartialEq)]
pub enum GameCommand {
    Start,
    Console {
        expected_generation: GenerationId,
        line: String,
    },
    Moderate {
        expected_generation: GenerationId,
        action: ModerationAction,
    },
    Stop {
        expected_generation: GenerationId,
    },
    Sleep {
        expected_generation: GenerationId,
        expected_cursor: u64,
    },
    Restart {
        expected_generation: GenerationId,
    },
    ForceKill {
        expected_generation: GenerationId,
    },
}
impl std::fmt::Debug for GameCommand {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.operation_type())
    }
}
impl GameCommand {
    pub(super) const fn operation_type(&self) -> &'static str {
        match self {
            Self::Start => "game.start",
            Self::Console { .. } => "game.console",
            Self::Moderate { .. } => "players.moderate",
            Self::Stop { .. } => "game.stop",
            Self::Sleep { .. } => "game.sleep",
            Self::Restart { .. } => "game.restart",
            Self::ForceKill { .. } => "game.force_kill",
        }
    }
    pub(super) fn canonical_input(&self) -> OperationsResult<String> {
        match self {
            Self::Start => Ok("{}".to_owned()),
            Self::Sleep {
                expected_generation,
                expected_cursor,
            } => serde_json::to_string(&SleepInput {
                expected_generation: expected_generation.to_hex(),
                expected_cursor: *expected_cursor,
            })
            .map_err(|_| invalid()),
            Self::Moderate {
                expected_generation,
                action,
            } => {
                let action = action.clone().validated().map_err(|_| invalid())?;
                serde_json::to_string(&ModerationInput {
                    expected_generation: expected_generation.to_hex(),
                    action,
                })
                .map_err(|_| invalid())
            }
            Self::Console {
                expected_generation,
                line,
            } => {
                if line.is_empty() || line.len() > 1024 || line.chars().any(char::is_control) {
                    return Err(invalid());
                }
                serde_json::to_string(&ConsoleInput {
                    expected_generation: expected_generation.to_hex(),
                    line: line.clone(),
                })
                .map_err(|_| invalid())
            }
            Self::Stop {
                expected_generation,
            }
            | Self::Restart {
                expected_generation,
            }
            | Self::ForceKill {
                expected_generation,
            } => serde_json::to_string(&GenerationInput {
                expected_generation: expected_generation.to_hex(),
            })
            .map_err(|_| invalid()),
        }
    }
}

/// Constructed only inside the concrete game composition. This is not a request field
/// or authorization proof; the process owner still rechecks the exact generation at effect time.
#[derive(Clone, Eq, PartialEq)]
pub(crate) struct SourceRevision(String);
impl SourceRevision {
    pub(super) fn for_properties(hash: &str) -> Self {
        Self(format!("properties-sha256:{hash}"))
    }
    pub(super) fn for_game(generation: Option<GenerationId>) -> Self {
        Self(format!(
            "game-generation:{}",
            generation.map_or_else(|| "none".to_owned(), GenerationId::to_hex)
        ))
    }
    pub(super) fn as_str(&self) -> &str {
        &self.0
    }
}
impl std::fmt::Debug for SourceRevision {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SourceRevision[<redacted>]")
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StartInput {}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct SleepInput {
    expected_generation: String,
    expected_cursor: u64,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct GenerationInput {
    expected_generation: String,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ConsoleInput {
    expected_generation: String,
    line: String,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ModerationInput {
    expected_generation: String,
    action: ModerationAction,
}

// Serde's derived struct visitors can also accept positional sequences. The
// durable lifecycle contract is an object, never a second array representation.
// This is only a root-shape guard; serde still validates the whole JSON text,
// duplicate fields, exact types and denied unknown fields.
fn object_input<T: serde::de::DeserializeOwned>(raw: &str) -> OperationsResult<T> {
    if !raw.trim_start().starts_with('{') {
        return Err(invalid());
    }
    serde_json::from_str(raw).map_err(|_| invalid())
}

pub(super) fn derive(kind: &str, raw: &str) -> OperationsResult<Option<PreparedOperation>> {
    let command = match kind {
        "players.moderate" => {
            let input: ModerationInput = object_input(raw)?;
            GameCommand::Moderate {
                expected_generation: GenerationId::parse_hex(&input.expected_generation)
                    .map_err(|_| invalid())?,
                action: input.action.validated().map_err(|_| invalid())?,
            }
        }
        "game.console" => {
            let input: ConsoleInput = object_input(raw)?;
            GameCommand::Console {
                expected_generation: GenerationId::parse_hex(&input.expected_generation)
                    .map_err(|_| invalid())?,
                line: input.line,
            }
        }
        "game.start" => {
            let _: StartInput = object_input(raw)?;
            GameCommand::Start
        }
        "game.sleep" => {
            let input: SleepInput = object_input(raw)?;
            GameCommand::Sleep {
                expected_generation: GenerationId::parse_hex(&input.expected_generation)
                    .map_err(|_| invalid())?,
                expected_cursor: input.expected_cursor,
            }
        }
        "game.stop" | "game.restart" | "game.force_kill" => {
            let input: GenerationInput = object_input(raw)?;
            let expected_generation =
                GenerationId::parse_hex(&input.expected_generation).map_err(|_| invalid())?;
            if kind == "game.stop" {
                GameCommand::Stop {
                    expected_generation,
                }
            } else if kind == "game.restart" {
                GameCommand::Restart {
                    expected_generation,
                }
            } else {
                GameCommand::ForceKill {
                    expected_generation,
                }
            }
        }
        _ => return Ok(None),
    };
    let canonical_input = command.canonical_input()?;
    let (capability, target, effect_idempotent, requires_confirmation) = match &command {
        GameCommand::Console {
            expected_generation,
            ..
        } => (
            Capability::ConsoleWrite,
            format!("game:generation:{}", expected_generation.to_hex()),
            false,
            false,
        ),
        GameCommand::Moderate {
            expected_generation,
            action,
        } => (
            action.capability(),
            format!("game:generation:{}", expected_generation.to_hex()),
            false,
            matches!(action, ModerationAction::Op { .. }),
        ),
        GameCommand::Start => (Capability::LifecycleStart, "game".to_owned(), false, false),
        GameCommand::Stop {
            expected_generation,
        } => (
            Capability::LifecycleStop,
            format!("game:generation:{}", expected_generation.to_hex()),
            true,
            false,
        ),
        GameCommand::Sleep {
            expected_generation,
            ..
        } => (
            Capability::LifecycleStop,
            format!("game:generation:{}", expected_generation.to_hex()),
            true,
            false,
        ),
        GameCommand::Restart {
            expected_generation,
        } => (
            Capability::LifecycleRestart,
            format!("game:generation:{}", expected_generation.to_hex()),
            false,
            false,
        ),
        GameCommand::ForceKill {
            expected_generation,
        } => (
            Capability::LifecycleForceKill,
            format!("game:generation:{}", expected_generation.to_hex()),
            false,
            true,
        ),
    };
    let mut requirements = vec![Requirement {
        capability,
        resource: None,
    }];
    if matches!(command, GameCommand::Sleep { .. }) {
        requirements.extend(
            [Capability::ConsoleRead, Capability::PlayersView].map(|capability| Requirement {
                capability,
                resource: None,
            }),
        );
    }
    Ok(Some(PreparedOperation {
        operation_type: kind.to_owned(),
        payload_hash: digest(&canonical_input),
        canonical_input,
        target,
        requirements,
        resources: vec![GAME_LOCK.to_owned()],
        effect_idempotent,
        requires_confirmation,
        command: Some(command),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn invalid_input(kind: &str, input: &str) {
        assert_eq!(
            derive(kind, input)
                .err()
                .expect("refuse malformed lifecycle input")
                .code(),
            "invalid_request"
        );
    }

    #[test]
    fn start_requires_an_empty_object_not_positional_or_extra_fields() {
        for input in ["[]", "null", "\"{}\"", "{\"argv\":[]}", "{}{}"] {
            invalid_input("game.start", input);
        }
        let plan = derive("game.start", " \n { } \t")
            .expect("object JSON")
            .expect("closed Start");
        assert_eq!(plan.canonical_input, "{}");
        assert_eq!(plan.target, "game");
    }

    #[test]
    fn generation_input_is_exact_and_has_no_secondary_array_representation() {
        for kind in ["game.stop", "game.force_kill"] {
            for input in [
                "{}",
                "[]",
                "[\"00112233445566778899aabbccddeeff\"]",
                "{\"expected_generation\":1}",
                "{\"expected_generation\":\"bad\"}",
                "{\"expected_generation\":\"00112233445566778899aabbccddeeff\",\"pid\":42}",
                "{\"expected_generation\":\"00112233445566778899aabbccddeeff\",\"expected_generation\":\"00112233445566778899aabbccddeeff\"}",
            ] {
                invalid_input(kind, input);
            }
        }
    }

    #[test]
    fn moderation_binding_is_closed_canonical_and_derives_authority() {
        let generation = GenerationId::parse_hex("00112233445566778899aabbccddeeff").unwrap();
        for (action, capability, confirmation) in [
            (
                ModerationAction::Kick {
                    player: "Alex".into(),
                    reason: Some("why".into()),
                },
                Capability::PlayersModerate,
                false,
            ),
            (
                ModerationAction::Op {
                    player: "Alex".into(),
                },
                Capability::PlayersOperator,
                true,
            ),
            (
                ModerationAction::Deop {
                    player: "Alex".into(),
                },
                Capability::PlayersOperator,
                false,
            ),
        ] {
            let command = GameCommand::Moderate {
                expected_generation: generation,
                action,
            };
            let raw = command.canonical_input().unwrap();
            let plan = derive("players.moderate", &raw).unwrap().unwrap();
            assert_eq!(plan.command, Some(command));
            assert_eq!(plan.requirements[0].capability, capability);
            assert_eq!(plan.resources, vec![GAME_LOCK.to_owned()]);
            assert_eq!(plan.requires_confirmation, confirmation);
            assert!(!plan.effect_idempotent);
            assert_eq!(
                derive("players.moderate", &plan.canonical_input)
                    .unwrap()
                    .unwrap()
                    .canonical_input,
                raw
            );
        }
        for input in [
            "[]",
            r#"{"expected_generation":"00112233445566778899aabbccddeeff","action":{"action":"kick","player":"@a"}}"#,
            r#"{"expected_generation":"00112233445566778899aabbccddeeff","action":{"action":"op","player":"Alex","reason":"why"}}"#,
            r#"{"expected_generation":"00112233445566778899aabbccddeeff","action":{"action":"pardon","player":"Alex"},"capability":"console.write"}"#,
        ] {
            invalid_input("players.moderate", input);
        }
    }

    #[test]
    fn closed_commands_round_trip_exact_server_owned_security_contracts() {
        let generation =
            GenerationId::parse_hex("00112233445566778899aabbccddeeff").expect("lookup format");
        for (command, capability, idempotent, confirmation) in [
            (GameCommand::Start, Capability::LifecycleStart, false, false),
            (
                GameCommand::Stop {
                    expected_generation: generation,
                },
                Capability::LifecycleStop,
                true,
                false,
            ),
            (
                GameCommand::ForceKill {
                    expected_generation: generation,
                },
                Capability::LifecycleForceKill,
                false,
                true,
            ),
        ] {
            let canonical = command.canonical_input().expect("bounded typed input");
            let plan = derive(command.operation_type(), &canonical)
                .expect("valid binding")
                .expect("closed inventory");
            let again = derive(command.operation_type(), &plan.canonical_input)
                .expect("stable")
                .expect("closed");
            assert_eq!(plan.command, Some(command));
            assert_eq!(plan.canonical_input, canonical);
            assert_eq!(plan.payload_hash, digest(&canonical));
            assert_eq!(plan.resources, vec![GAME_LOCK.to_owned()]);
            assert_eq!(plan.effect_idempotent, idempotent);
            assert_eq!(plan.requires_confirmation, confirmation);
            assert!(
                plan.requirements.len() == 1
                    && plan.requirements[0].capability == capability
                    && plan.requirements[0].resource.is_none()
            );
            assert_eq!(again.canonical_input, plan.canonical_input);
            assert_eq!(again.target, plan.target);
            assert_eq!(again.resources, plan.resources);
            assert!(again.requirements == plan.requirements);
        }
        assert!(
            derive("game.unknown", "{}")
                .expect("unknown inventory")
                .is_none()
        );
    }
}
