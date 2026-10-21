//! Durable management branding. It does not edit pack metadata or Minecraft files.
use super::*;
use serde::Deserializer;

const BRANDING: &str = "server.branding";
const ICON: &str = "server.branding.icon";
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
        return Ok(json!({"name":null,"description":null,"revision":"0"}));
    };
    let mut value: Value =
        serde_json::from_str(&value).map_err(|_| PermissionError::Unavailable)?;
    value["revision"] = json!(revision.to_string());
    Ok(value)
}
pub(super) fn patch(repo: &Repository<'_>, input: &ServerPatch) -> AdminResult<Value> {
    if input.name.is_none() && input.description.is_none() {
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
    if value["revision"] != expected.to_string() {
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
    save(repo, BRANDING, &value.to_string())?;
    projection(repo)
}
pub(super) fn icon(repo: &Repository<'_>) -> AdminResult<Option<Vec<u8>>> {
    setting(repo, ICON)?
        .map(|(value, _)| {
            URL_SAFE_NO_PAD
                .decode(value)
                .map_err(|_| PermissionError::Unavailable)
        })
        .transpose()
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
pub(super) fn put_icon(
    repo: &Repository<'_>,
    expected: Option<&str>,
    bytes: &[u8],
) -> AdminResult<Value> {
    let normalized =
        crate::motd::normalize_branding_icon(bytes).ok_or(PermissionError::InvalidRequest)?;
    condition(icon(repo)?.as_deref(), expected)?;
    save(repo, ICON, &URL_SAFE_NO_PAD.encode(&normalized))?;
    Ok(json!({"etag":icon_etag(&normalized)}))
}
pub(super) fn delete_icon(repo: &Repository<'_>, expected: &str) -> AdminResult<Value> {
    let current = icon(repo)?.ok_or(PermissionError::NotFound)?;
    condition(Some(&current), Some(expected))?;
    repo.connection().execute(
        "DELETE FROM settings WHERE server_id=? AND key=?",
        params![repo_server(repo)?, ICON],
    )?;
    Ok(json!({"deleted":true}))
}
