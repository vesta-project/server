//! Closed Java Edition moderation arguments and bounded player-file projections.
//! No player impersonation or caller-selected command verbs are supported.
use serde::{Deserialize, Serialize};
pub const DEFAULT_BAN_REASON: &str = "Banned by an operator.";

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum ModerationAction {
    Kick {
        player: String,
        reason: Option<String>,
    },
    Ban {
        player: String,
        reason: Option<String>,
        duration_seconds: Option<u64>,
    },
    Pardon {
        player: String,
    },
    WhitelistAdd {
        player: String,
    },
    WhitelistRemove {
        player: String,
    },
    Op {
        player: String,
    },
    Deop {
        player: String,
    },
    Kill {
        player: String,
    },
    Lightning {
        player: String,
    },
}
impl ModerationAction {
    pub fn player(&self) -> &str {
        match self {
            Self::Kick { player, .. }
            | Self::Ban { player, .. }
            | Self::Pardon { player }
            | Self::WhitelistAdd { player }
            | Self::WhitelistRemove { player }
            | Self::Op { player }
            | Self::Deop { player }
            | Self::Kill { player }
            | Self::Lightning { player } => player,
        }
    }
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::Kick { .. } => "kick",
            Self::Ban { .. } => "ban",
            Self::Pardon { .. } => "pardon",
            Self::WhitelistAdd { .. } => "whitelist_add",
            Self::WhitelistRemove { .. } => "whitelist_remove",
            Self::Op { .. } => "op",
            Self::Deop { .. } => "deop",
            Self::Kill { .. } => "kill",
            Self::Lightning { .. } => "lightning",
        }
    }
    pub fn reason(&self) -> Option<&str> {
        match self {
            Self::Kick { reason, .. } | Self::Ban { reason, .. } => reason.as_deref(),
            _ => None,
        }
    }
    pub const fn duration_seconds(&self) -> Option<u64> {
        match self {
            Self::Ban {
                duration_seconds, ..
            } => *duration_seconds,
            _ => None,
        }
    }
    pub const fn capability(&self) -> crate::identity::Capability {
        match self {
            Self::Op { .. } | Self::Deop { .. } => crate::identity::Capability::PlayersOperator,
            _ => crate::identity::Capability::PlayersModerate,
        }
    }
    /// Reject line separators before trimming: even a discarded control is invalid input.
    pub fn validated(mut self) -> Result<Self, &'static str> {
        if !valid_player(self.player()) {
            return Err("invalid_request");
        }
        if let Self::Kick { reason, .. } | Self::Ban { reason, .. } = &mut self
            && let Some(value) = reason
        {
            if value
                .chars()
                .any(|c| c.is_control() || matches!(c, '\u{2028}' | '\u{2029}'))
            {
                return Err("invalid_request");
            }
            *value = value.trim().to_owned();
            if !(1..=256).contains(&value.chars().count()) || value.len() > 512 {
                return Err("invalid_request");
            }
        }
        if self
            .duration_seconds()
            .is_some_and(|v| !(60..=31_536_000).contains(&v))
        {
            return Err("invalid_request");
        }
        Ok(self)
    }
    /// Only validated fields are rendered; reasons are a single trailing argument.
    pub fn render(&self) -> Result<String, &'static str> {
        let action = self.clone().validated()?;
        let player = action.player();
        let mut line = match action {
            Self::Kick { .. } => format!("kick {player}"),
            Self::Ban { .. } => format!("ban {player}"),
            Self::Pardon { .. } => format!("pardon {player}"),
            Self::WhitelistAdd { .. } => format!("whitelist add {player}"),
            Self::WhitelistRemove { .. } => format!("whitelist remove {player}"),
            Self::Op { .. } => format!("op {player}"),
            Self::Deop { .. } => format!("deop {player}"),
            Self::Kill { .. } => format!("kill {player}"),
            Self::Lightning { .. } => {
                format!("execute at {player} run summon minecraft:lightning_bolt ~ ~ ~")
            }
        };
        let reason = action
            .reason()
            .or_else(|| matches!(action, Self::Ban { .. }).then_some(DEFAULT_BAN_REASON));
        if let Some(reason) = reason {
            line.push(' ');
            line.push_str(reason);
        }
        debug_assert!(
            line.len() <= 1024,
            "typed lines fit the unchanged native bound"
        );
        Ok(line)
    }
}
pub fn valid_player(player: &str) -> bool {
    (1..=16).contains(&player.len())
        && player
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct BanEntry {
    pub name: String,
    pub uuid: String,
    pub reason: String,
    pub created: String,
    pub expires: String,
    pub source: String,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OpEntry {
    pub name: String,
    pub uuid: String,
    pub level: u8,
    pub bypasses_player_limit: bool,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct WhitelistEntry {
    pub name: String,
    pub uuid: String,
}

fn load<T: serde::de::DeserializeOwned>(path: &std::path::Path) -> Result<Vec<T>, &'static str> {
    let bytes = match crate::files::read_local_document(path, 1024 * 1024) {
        Ok(bytes) => bytes,
        Err(crate::files::FileError::NotFound) => return Ok(Vec::new()),
        Err(crate::files::FileError::TooLarge) => return Err("players_file_too_large"),
        Err(_) => return Err("players_file_unavailable"),
    };
    let items: Vec<T> = serde_json::from_slice(&bytes).map_err(|_| "players_file_malformed")?;
    if items.len() > 10_000 {
        return Err("players_file_too_large");
    }
    Ok(items)
}
/// Server-written entries are displayed, never rendered into commands, so names are
/// only bounded (offline-mode servers may store names outside the Java Edition set).
fn entry_valid(name: &str, uuid: &str) -> bool {
    (1..=64).contains(&name.chars().count())
        && !name.chars().any(char::is_control)
        && crate::identity::minecraft::MinecraftUuid::parse(uuid).is_ok()
}
pub fn bans(directory: &std::path::Path) -> Result<Vec<BanEntry>, &'static str> {
    let items: Vec<BanEntry> = load(&directory.join("banned-players.json"))?;
    if items.iter().any(|v| !entry_valid(&v.name, &v.uuid)) {
        return Err("players_file_malformed");
    }
    Ok(items)
}
pub fn ops(directory: &std::path::Path) -> Result<Vec<OpEntry>, &'static str> {
    let items: Vec<OpEntry> = load(&directory.join("ops.json"))?;
    if items
        .iter()
        .any(|v| !entry_valid(&v.name, &v.uuid) || !(1..=4).contains(&v.level))
    {
        return Err("players_file_malformed");
    }
    Ok(items)
}
pub fn whitelist(directory: &std::path::Path) -> Result<Vec<WhitelistEntry>, &'static str> {
    // A fresh persisted-file observation, not Minecraft's live in-memory list.
    // Direct operator edits may require a game-side reload before join checks use them.
    let items: Vec<WhitelistEntry> = load(&directory.join("whitelist.json"))?;
    if items.iter().any(|v| !entry_valid(&v.name, &v.uuid)) {
        return Err("players_file_malformed");
    }
    Ok(items)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn whitelist_observations_reread_disk_and_do_not_predict_command_effects() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("whitelist.json");
        assert!(whitelist(root.path()).unwrap().is_empty());
        std::fs::write(
            &path,
            br#"[{"name":"Alex","uuid":"853c80ef-3c37-49fd-aa49-938b674adae6"}]"#,
        )
        .unwrap();
        assert_eq!(whitelist(root.path()).unwrap()[0].name, "Alex");
        // Merely preparing an approved command must not optimistically change
        // what Vesta reports from the game's persisted document.
        ModerationAction::WhitelistRemove {
            player: "Alex".into(),
        }
        .render()
        .unwrap();
        assert_eq!(whitelist(root.path()).unwrap().len(), 1);
        std::fs::write(path, b"[]").unwrap();
        assert!(whitelist(root.path()).unwrap().is_empty());
    }
    #[test]
    fn moderation_refuses_injection_and_bounds() {
        for player in ["@a", "a b", "a\nstop", "a;op", "", "abcdefghijklmnopq", "é"] {
            assert!(
                ModerationAction::Pardon {
                    player: player.into()
                }
                .render()
                .is_err()
            );
        }
        for reason in [
            "\nstop",
            "x\rstop",
            "x\u{2028}stop",
            "x\u{2029}stop",
            "\twhy",
            " ",
        ] {
            assert!(
                ModerationAction::Kick {
                    player: "Alex".into(),
                    reason: Some(reason.into())
                }
                .render()
                .is_err()
            );
        }
        for duration in [0, 59, 31_536_001, u64::MAX] {
            assert!(
                ModerationAction::Ban {
                    player: "Alex".into(),
                    reason: None,
                    duration_seconds: Some(duration)
                }
                .validated()
                .is_err()
            );
        }
        assert!(
            ModerationAction::Kick {
                player: "Alex".into(),
                reason: Some("x".repeat(257))
            }
            .validated()
            .is_err()
        );
    }
    #[test]
    fn server_written_entries_allow_offline_names_but_stay_bounded() {
        let temp = tempfile::tempdir().unwrap();
        let uuid = "123e4567-e89b-42d3-a456-426614174000";
        std::fs::write(
            temp.path().join("whitelist.json"),
            format!(r#"[{{"name":"Offline-Name.1","uuid":"{uuid}"}}]"#),
        )
        .unwrap();
        assert_eq!(whitelist(temp.path()).unwrap()[0].name, "Offline-Name.1");
        for name in [String::new(), "x".repeat(65), "a\nb".into()] {
            std::fs::write(
                temp.path().join("whitelist.json"),
                serde_json::json!([{"name": name, "uuid": uuid}]).to_string(),
            )
            .unwrap();
            assert_eq!(
                whitelist(temp.path()).unwrap_err(),
                "players_file_malformed"
            );
        }
        // Commands still accept only Java Edition names.
        assert!(!valid_player("Offline-Name.1"));
    }

    #[test]
    fn moderation_unicode_reason_byte_boundary_fits_native_write() {
        let reason = "\u{1f600}".repeat(128);
        assert_eq!(reason.len(), 512);
        for action in [
            ModerationAction::Kick {
                player: "abcdefghijklmnop".into(),
                reason: Some(reason.clone()),
            },
            ModerationAction::Ban {
                player: "abcdefghijklmnop".into(),
                reason: Some(reason.clone()),
                duration_seconds: None,
            },
            ModerationAction::Lightning {
                player: "abcdefghijklmnop".into(),
            },
        ] {
            assert!(action.render().unwrap().len() <= 1024);
        }
        assert!(
            ModerationAction::Kick {
                player: "Alex".into(),
                reason: Some("\u{1f600}".repeat(129))
            }
            .render()
            .is_err()
        );
        assert!(
            ModerationAction::Kick {
                player: "Alex".into(),
                reason: Some("x".repeat(256))
            }
            .render()
            .is_ok()
        );
    }

    #[test]
    fn moderation_rendering_is_fixed_and_never_impersonates() {
        let samples = [
            ("kick", "kick Alex"),
            ("ban", "ban Alex Banned by an operator."),
            ("pardon", "pardon Alex"),
            ("whitelist_add", "whitelist add Alex"),
            ("whitelist_remove", "whitelist remove Alex"),
            ("op", "op Alex"),
            ("deop", "deop Alex"),
            ("kill", "kill Alex"),
            (
                "lightning",
                "execute at Alex run summon minecraft:lightning_bolt ~ ~ ~",
            ),
        ];
        for (kind, expected) in samples {
            let value: ModerationAction =
                serde_json::from_value(serde_json::json!({"action":kind,"player":"Alex"})).unwrap();
            assert_eq!(value.render().unwrap(), expected);
        }
        let action = ModerationAction::Ban {
            player: "Alex".into(),
            reason: Some("  /op @a; execute as @a run stop  ".into()),
            duration_seconds: Some(60),
        };
        assert_eq!(
            action.render().unwrap(),
            "ban Alex /op @a; execute as @a run stop"
        );
        assert!(
            serde_json::from_str::<ModerationAction>(
                r#"{"action":"pardon","player":"Alex","reason":"x"}"#
            )
            .is_err()
        );
        assert!(
            serde_json::from_str::<ModerationAction>(
                r#"{"action":"execute_as_player","player":"Alex"}"#
            )
            .is_err()
        );
    }
}
