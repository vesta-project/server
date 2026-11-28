//! Durable management branding. It does not edit pack metadata or Minecraft files.
use super::*;
use serde::Deserializer;

const BRANDING: &str = "server.branding";
const ICON: &str = "server.branding.icon";
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IconSource {
    #[default]
    Launcher,
    Game,
}

pub const MAX_ICON_BYTES: usize = 256 * 1024;
fn present_string<'de, D: Deserializer<'de>>(
    d: D,
) -> std::result::Result<Option<String>, D::Error> {
    String::deserialize(d).map(Some)
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ServerPatch {
    pub expected_revision: String,
    #[serde(default, deserialize_with = "present_string")]
    pub name: Option<String>,
    #[serde(default, deserialize_with = "present_string")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon_source: Option<IconSource>,
}
fn setting(repo: &Repository<'_>, key: &str) -> AdminResult<Option<(String, i64)>> {
    Ok(repo
        .connection()
        .query_row(
            "SELECT desired_value,desired_revision FROM settings WHERE server_id=? AND key=?",
            params![repo_server(repo)?, key],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?)
}
fn save(repo: &Repository<'_>, key: &str, value: &str) -> AdminResult<()> {
    repo.connection().execute("INSERT INTO settings(server_id,key,desired_value,desired_revision,apply_mode,effective_value,effective_revision,updated_at) VALUES(?,?,?,1,'immediate',?,1,?) ON CONFLICT(server_id,key) DO UPDATE SET desired_value=excluded.desired_value,desired_revision=settings.desired_revision+1,effective_value=excluded.desired_value,effective_revision=settings.desired_revision+1,updated_at=excluded.updated_at",params![repo_server(repo)?,key,value,value,repo.now])?;
    Ok(())
}
pub(super) fn projection(repo: &Repository<'_>) -> AdminResult<Value> {
    let Some((value, revision)) = setting(repo, BRANDING)? else {
        return Ok(json!({"name":null,"description":null,"revision":"0","icon_source":"launcher"}));
    };
    let mut value: Value =
        serde_json::from_str(&value).map_err(|_| PermissionError::Unavailable)?;
    value["revision"] = json!(revision.to_string());
    if value.get("icon_source").is_none() {
        value["icon_source"] = json!(IconSource::Launcher);
    }
    Ok(value)
}
pub(super) fn patch(repo: &Repository<'_>, input: &ServerPatch) -> AdminResult<Value> {
    if input.name.is_none() && input.description.is_none() && input.icon_source.is_none() {
        return Err(PermissionError::InvalidRequest);
    }
    if input.name.as_ref().is_some_and(|v| {
        v.is_empty()
            || v.len() > 512
            || v.chars().count() > 128
            || v.chars().all(char::is_whitespace)
            || v.chars().any(char::is_control)
    }) || input.description.as_ref().is_some_and(|v| {
        v.len() > 4096 || v.chars().count() > 1024 || v.chars().any(|c| c.is_control() && c != '\n')
    }) {
        return Err(PermissionError::InvalidRequest);
    }
    let expected = input
        .expected_revision
        .parse::<i64>()
        .ok()
        .filter(|v| *v >= 0 && v.to_string() == input.expected_revision)
        .ok_or(PermissionError::InvalidRequest)?;
    let mut value = projection(repo)?;
    if value["revision"].as_str() != Some(input.expected_revision.as_str()) {
        return Err(PermissionError::PreconditionFailed);
    }
    if expected == i64::MAX {
        return Err(PermissionError::Saturated);
    }
    value
        .as_object_mut()
        .expect("branding object")
        .remove("revision");
    if let Some(name) = &input.name {
        value["name"] = json!(name);
    }
    if let Some(description) = &input.description {
        value["description"] = json!(description);
    }
    if let Some(source) = input.icon_source {
        value["icon_source"] = json!(source);
    }
    save(repo, BRANDING, &value.to_string())?;
    projection(repo)
}
pub(super) fn icon(
    repo: &Repository<'_>,
    game: Option<&std::path::Path>,
) -> AdminResult<Option<Vec<u8>>> {
    if let Some(game) = game
        && vesta_state::server_icon::deleted_on_disk(repo.connection(), game)
            .map_err(|_| PermissionError::Unavailable)?
    {
        return Ok(None);
    }
    setting(repo, ICON)?
        .filter(|(value, _)| !value.is_empty())
        .map(|(value, _)| {
            URL_SAFE_NO_PAD
                .decode(value)
                .map_err(|_| PermissionError::Unavailable)
        })
        .transpose()
}
/// Host-side observation for server cards: the stored icon's ETag, read
/// directly from the server's own database without any caller authority.
pub fn stored_icon_etag(
    db: &vesta_state::StateDatabase,
    game: Option<&std::path::Path>,
) -> Option<String> {
    let encoded: String = db
        .read(|c| {
            if let Some(game) = game
                && vesta_state::server_icon::deleted_on_disk(c, game)?
            {
                return Ok(None);
            }
            Ok(c.query_row(
                "SELECT s.desired_value FROM settings s JOIN server_identity i ON i.server_id=s.server_id WHERE i.singleton=1 AND s.key=?1",
                [ICON],
                |r| r.get(0),
            )
            .optional()?)
        })
        .ok()??;
    (!encoded.is_empty())
        .then(|| URL_SAFE_NO_PAD.decode(encoded).ok())
        .flatten()
        .map(|b| icon_etag(&b))
}
pub(crate) fn stored_icon_source(db: &vesta_state::StateDatabase) -> IconSource {
    db.read(|c| {
        Ok(c.query_row(
            "SELECT s.desired_value FROM settings s JOIN server_identity i ON i.server_id=s.server_id WHERE i.singleton=1 AND s.key=?1",
            [BRANDING],
            |r| r.get::<_, String>(0),
        ).optional()?)
    })
    .ok()
    .flatten()
    .and_then(|text| serde_json::from_str::<Value>(&text).ok())
    .and_then(|value| serde_json::from_value(value["icon_source"].clone()).ok())
    .unwrap_or_default()
}

pub fn icon_etag(bytes: &[u8]) -> String {
    format!(
        "\"{}\"",
        hex::encode(digest::digest(&digest::SHA256, bytes))
    )
}
fn condition(current: Option<&[u8]>, expected: Option<&str>) -> AdminResult<()> {
    match (current, expected) {
        (None, None) => Ok(()),
        (Some(bytes), Some(expected)) if icon_etag(bytes) == expected => Ok(()),
        _ => Err(PermissionError::PreconditionFailed),
    }
}
/// Apply branding already saved before icon synchronization was introduced.
pub(crate) fn prepare_game_projection(
    c: &rusqlite::Connection,
    server: &str,
    game: &std::path::Path,
) -> vesta_state::Result<bool> {
    let Some(png) = game_projection(c, server)? else {
        return Ok(false);
    };
    vesta_state::server_icon::store_branding(c, game, png.as_deref(), false)?;
    Ok(true)
}

/// Outer None means the server still uses the legacy presentation icon.
pub(crate) fn game_projection(
    c: &rusqlite::Connection,
    server: &str,
) -> vesta_state::Result<Option<Option<Vec<u8>>>> {
    let setting: Option<String> = c
        .query_row(
            "SELECT desired_value FROM settings WHERE server_id=? AND key=?",
            params![server, ICON],
            |row| row.get(0),
        )
        .optional()?;
    Ok(setting.map(|encoded| {
        URL_SAFE_NO_PAD
            .decode(encoded)
            .ok()
            .and_then(|bytes| crate::motd::branding_game_icon(&bytes))
    }))
}

pub(super) fn put_icon(
    repo: &Repository<'_>,
    game: Option<&std::path::Path>,
    expected: Option<&str>,
    bytes: &[u8],
) -> AdminResult<Value> {
    let normalized =
        crate::motd::normalize_branding_icon(bytes).ok_or(PermissionError::InvalidRequest)?;
    condition(icon(repo, game)?.as_deref(), expected)?;
    if let Some(game) = game {
        let game_icon =
            crate::motd::branding_game_icon(&normalized).ok_or(PermissionError::InvalidRequest)?;
        vesta_state::server_icon::store_branding(repo.connection(), game, Some(&game_icon), true)
            .map_err(|_| PermissionError::Unavailable)?;
    }
    save(repo, ICON, &URL_SAFE_NO_PAD.encode(&normalized))?;
    Ok(json!({"etag":icon_etag(&normalized)}))
}
pub(super) fn delete_icon(
    repo: &Repository<'_>,
    game: Option<&std::path::Path>,
    expected: &str,
) -> AdminResult<Value> {
    let current = icon(repo, game)?;
    if let Some(current) = &current {
        condition(Some(current), Some(expected))?;
    }
    if let Some(game) = game {
        let cleared = vesta_state::server_icon::clear_branding(repo.connection(), game, |manual| {
            current.is_some() || condition(manual, Some(expected)).is_ok()
        })
        .map_err(|_| PermissionError::Unavailable)?;
        if !cleared {
            return Err(PermissionError::PreconditionFailed);
        }
    } else if current.is_none() {
        return Err(PermissionError::NotFound);
    }
    // Keep a tombstone so an old MOTD icon cannot reappear on restart.
    save(repo, ICON, "")?;
    Ok(json!({"deleted":true}))
}
