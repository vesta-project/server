//! Owner-controlled host settings persisted in the host database.
use super::{
    HostState,
    startup::StartupAction,
    owners::{Actor, require_owner},
    state::{changed, reject},
};
use crate::{IdentityErrorCode as Code, Result};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

/// mDNS TXT `name` limit.
pub const MACHINE_NAME_MAX_BYTES: usize = 63;
pub const DEVICE_NAME_MAX_CHARS: usize = 64;
const DEFAULT_REMOVAL_RETENTION_DAYS: u16 = 7;
const MAX_REMOVAL_RETENTION_DAYS: u16 = 365;
const NAME_INPUT_MAX_BYTES: usize = 256;

/// How a device that is new to this host may join an account that already
/// holds a role: by fresh account proof alone, or only through an owner-approved
/// link code.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NewDeviceSignIn {
    #[default]
    Account,
    Approval,
}
impl NewDeviceSignIn {
    fn as_str(self) -> &'static str {
        match self {
            Self::Account => "account",
            Self::Approval => "approval",
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostSettings {
    /// Owner-chosen machine name; `None` keeps the host.toml name.
    pub name: Option<String>,
    pub new_device_sign_in: NewDeviceSignIn,
    pub removed_server_retention_days: u16,
    pub default_on_agent_start: StartupAction,
}
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HostSettingsPatch {
    pub name: Option<String>,
    pub new_device_sign_in: Option<NewDeviceSignIn>,
    pub removed_server_retention_days: Option<u16>,
    pub default_on_agent_start: Option<StartupAction>,
}

/// Removes control and bidirectional-override characters, collapses whitespace
/// and trims. `None` for oversized input or nothing left.
fn sanitize(input: &str) -> Option<String> {
    if input.len() > NAME_INPUT_MAX_BYTES {
        return None;
    }
    let mut out = String::new();
    let mut space = false;
    for c in input.chars() {
        if c.is_whitespace() {
            space = !out.is_empty();
            continue;
        }
        if c.is_control()
            || matches!(c, '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2060}'..='\u{2069}' | '\u{FEFF}')
        {
            continue;
        }
        if space {
            out.push(' ');
            space = false;
        }
        out.push(c);
    }
    (!out.is_empty()).then_some(out)
}
/// Client-supplied device label, truncated to [`DEVICE_NAME_MAX_CHARS`].
pub fn device_name(input: &str) -> Option<String> {
    let name: String = sanitize(input)?
        .chars()
        .take(DEVICE_NAME_MAX_CHARS)
        .collect();
    Some(name.trim_end().to_owned())
}
/// Machine display name; rejected rather than truncated when too long.
pub fn machine_name(input: &str) -> Option<String> {
    sanitize(input).filter(|n| n.len() <= MACHINE_NAME_MAX_BYTES)
}

fn setting_in(c: &Connection, key: &str) -> Result<Option<String>> {
    Ok(c.query_row(
        "SELECT value FROM machine_settings WHERE key=?1",
        [key],
        |r| r.get(0),
    )
    .optional()?)
}
fn put_in(c: &Connection, key: &str, value: &str) -> Result<()> {
    c.execute(
        "INSERT INTO machine_settings(key,value) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
        params![key, value],
    )?;
    Ok(())
}
pub(crate) fn sign_in_mode_in(c: &Connection) -> Result<NewDeviceSignIn> {
    Ok(match setting_in(c, "new_device_sign_in")?.as_deref() {
        Some("approval") => NewDeviceSignIn::Approval,
        _ => NewDeviceSignIn::Account,
    })
}
pub(crate) fn removal_retention_in(c: &Connection) -> Result<u16> {
    match setting_in(c, "removed_server_retention_days")? {
        None => Ok(DEFAULT_REMOVAL_RETENTION_DAYS),
        Some(value) => value
            .parse::<u16>()
            .ok()
            .filter(|days| (1..=MAX_REMOVAL_RETENTION_DAYS).contains(days))
            .ok_or_else(|| reject(Code::InvalidRequest)),
    }
}

pub(super) fn startup_default_in(c: &Connection) -> Result<StartupAction> {
    setting_in(c, "default_on_agent_start")?.map(|value| {
        serde_json::from_str(&value).map_err(|_| reject(Code::InvalidRequest))
    }).transpose().map(Option::unwrap_or_default)
}

fn settings_in(c: &Connection) -> Result<HostSettings> {
    Ok(HostSettings {
        name: setting_in(c, "name")?,
        new_device_sign_in: sign_in_mode_in(c)?,
        removed_server_retention_days: removal_retention_in(c)?,
        default_on_agent_start: startup_default_in(c)?,
    })
}
impl HostState {
    pub fn settings(&self) -> Result<HostSettings> {
        self.read(settings_in)
    }
    /// Owner-only. Each change is audited. Settings do not change any existing
    /// grant, so the auth generation stays put.
    pub fn update_settings(&self, actor: Actor, patch: HostSettingsPatch) -> Result<HostSettings> {
        let name = patch
            .name
            .as_deref()
            .map(|n| machine_name(n).ok_or_else(|| reject(Code::InvalidRequest)))
            .transpose()?;
        if patch
            .removed_server_retention_days
            .is_some_and(|days| !(1..=MAX_REMOVAL_RETENTION_DAYS).contains(&days))
            || (name.is_none()
                && patch.new_device_sign_in.is_none()
                && patch.removed_server_retention_days.is_none()
                && patch.default_on_agent_start.is_none())
        {
            return Err(reject(Code::InvalidRequest));
        }
        self.write(|c| {
            require_owner(c, actor)?;
            if let Some(name) = &name {
                put_in(c, "name", name)?;
                changed(
                    c,
                    false,
                    "host_rename",
                    &actor.audit(),
                    "name",
                    self.now_ms(),
                )?;
            }
            if let Some(mode) = patch.new_device_sign_in {
                put_in(c, "new_device_sign_in", mode.as_str())?;
                changed(
                    c,
                    false,
                    "new_device_sign_in_change",
                    &actor.audit(),
                    mode.as_str(),
                    self.now_ms(),
                )?;
            }
            if let Some(days) = patch.removed_server_retention_days {
                let value = days.to_string();
                put_in(c, "removed_server_retention_days", &value)?;
                changed(
                    c,
                    false,
                    "removed_server_retention_change",
                    &actor.audit(),
                    &value,
                    self.now_ms(),
                )?;
            }
            if let Some(action) = patch.default_on_agent_start {
                let value = serde_json::to_string(&action).map_err(|_| reject(Code::InvalidRequest))?;
                put_in(c, "default_on_agent_start", &value)?;
                changed(c, false, "default_startup_change", &actor.audit(), "default_on_agent_start", self.now_ms())?;
            }
            settings_in(c)
        })
    }
}
