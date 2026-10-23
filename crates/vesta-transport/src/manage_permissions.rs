//! Strict wire adapter for transactional permission administration.
use super::{Request, decode, error, ok};
use crate::Reply;
use http::{Method, StatusCode};
use serde::Deserialize;
use vesta_control::{
    host::NetworkHost,
    identity::management::permissions::{
        AccessPatch, DeviceRename, Invitation, PermissionError, PermissionMutation,
        PermissionQuery, RoleInput, RolePatch, UserRoles,
        branding::{MAX_ICON_BYTES, ServerPatch, icon_etag},
    },
};

pub(super) fn implemented(path: &str) -> bool {
    let Some(action) = path.strip_prefix("/api/v3/manage/") else {
        return false;
    };
    matches!(
        action,
        "users" | "roles" | "access-sources" | "invitations" | "devices" | "server" | "server/icon"
    ) || resource(action, "users").is_some()
        || resource(action, "roles").is_some()
        || resource(action, "devices").is_some()
        || resource(action, "invitations").is_some()
        || user_child(action, "roles").is_some()
        || user_child(action, "devices").is_some()
}
fn resource<'a>(action: &'a str, kind: &str) -> Option<&'a str> {
    let (prefix, id) = action.split_once('/')?;
    (prefix == kind && !id.is_empty() && id.len() <= 96 && !id.contains('/')).then_some(id)
}
fn user_child<'a>(action: &'a str, child: &str) -> Option<&'a str> {
    resource(action.strip_suffix(&format!("/{child}"))?, "users")
}
fn failure(e: PermissionError) -> Reply {
    let (status, code) = match e {
        PermissionError::SessionInvalid => (StatusCode::UNAUTHORIZED, "management_session_invalid"),
        PermissionError::Forbidden => (StatusCode::FORBIDDEN, "management_forbidden"),
        PermissionError::InvalidRequest => (StatusCode::BAD_REQUEST, "invalid_request"),
        PermissionError::NotFound => (StatusCode::NOT_FOUND, "resource_not_found"),
        PermissionError::PreconditionFailed => {
            (StatusCode::PRECONDITION_FAILED, "precondition_failed")
        }
        PermissionError::IdempotencyConflict => (StatusCode::CONFLICT, "idempotency_conflict"),
        PermissionError::Conflict => (StatusCode::CONFLICT, "permission_conflict"),
        PermissionError::Saturated => (StatusCode::SERVICE_UNAVAILABLE, "temporarily_unavailable"),
        PermissionError::Unavailable => {
            (StatusCode::SERVICE_UNAVAILABLE, "temporarily_unavailable")
        }
    };
    error(status, code)
}
fn pagination(query: Option<&str>) -> Option<(u32, u16)> {
    let (mut offset, mut limit) = (None, None);
    if let Some(query) = query {
        if query.is_empty() || query.len() > 80 {
            return None;
        }
        for part in query.split('&') {
            let (k, v) = part.split_once('=')?;
            if v.is_empty()
                || !v.bytes().all(|b| b.is_ascii_digit())
                || (v.len() > 1 && v.starts_with('0'))
            {
                return None;
            }
            match k {
                "offset" if offset.is_none() => offset = Some(v.parse::<u32>().ok()?),
                "limit" if limit.is_none() => limit = Some(v.parse::<u16>().ok()?),
                _ => return None,
            }
        }
    }
    let (offset, limit) = (offset.unwrap_or(0), limit.unwrap_or(50));
    (offset <= 1000000 && (1..=100).contains(&limit)).then_some((offset, limit))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DeleteRole {
    expected_version: String,
}

pub(super) fn dispatch(host: &NetworkHost, request: Request<'_>) -> Reply {
    let Request {
        method,
        path,
        query,
        authorization,
        idempotency,
        bytes,
        ..
    } = request;
    let Some(token) = authorization else {
        return error(StatusCode::UNAUTHORIZED, "management_session_invalid");
    };
    // Session validation precedes parsing, and current capability admission is repeated in the transaction.
    if let Err(e) = host.management().authenticate(token) {
        return super::management_error(e);
    }
    let action = path
        .strip_prefix("/api/v3/manage/")
        .expect("management route");
    if action == "server/icon" {
        return icon(host, request, token);
    }
    if request.if_match.is_some()
        || request.if_none_match.is_some()
        || request.range.is_some()
        || request.if_range.is_some()
    {
        return error(StatusCode::BAD_REQUEST, "invalid_request");
    }
    if bytes.len() > 16384 {
        return error(StatusCode::PAYLOAD_TOO_LARGE, "request_too_large");
    }
    if method == Method::GET {
        if !bytes.is_empty() {
            return error(StatusCode::BAD_REQUEST, "invalid_request");
        }
        let query = if matches!(action, "users" | "devices" | "invitations") {
            let Some((offset, limit)) = pagination(query) else {
                return error(StatusCode::BAD_REQUEST, "invalid_request");
            };
            match action {
                "devices" => PermissionQuery::ManageableDevices { offset, limit },
                "invitations" => PermissionQuery::Invitations { offset, limit },
                _ => PermissionQuery::Users { offset, limit },
            }
        } else {
            if query.is_some() {
                return error(StatusCode::BAD_REQUEST, "invalid_request");
            }
            match action {
                "server" => PermissionQuery::Server,
                "roles" => PermissionQuery::Roles,
                "access-sources" => PermissionQuery::Access,
                _ => {
                    if let Some(id) = user_child(action, "devices") {
                        PermissionQuery::Devices(id.into())
                    } else if let Some(id) = resource(action, "users") {
                        PermissionQuery::User(id.into())
                    } else {
                        return error(StatusCode::BAD_REQUEST, "invalid_request");
                    }
                }
            }
        };
        return match host.management().permission_query(token, query) {
            Ok(v) => ok(if action == "server" {
                server(host, v)
            } else {
                v
            }),
            Err(e) => failure(e),
        };
    }
    if query.is_some() {
        return error(StatusCode::BAD_REQUEST, "invalid_request");
    }
    let command = match (method.as_str(), action) {
        ("PATCH", "server") => decode::<ServerPatch>(bytes).map(PermissionMutation::Server),
        ("POST", "roles") => decode::<RoleInput>(bytes).map(PermissionMutation::CreateRole),
        ("PATCH", "access-sources") => decode::<AccessPatch>(bytes).map(PermissionMutation::Access),
        ("POST", "invitations") => decode::<Invitation>(bytes).map(PermissionMutation::Invitation),
        ("PUT", _) => user_child(action, "roles").and_then(|id| {
            decode::<UserRoles>(bytes).map(|v| PermissionMutation::UserRoles(id.into(), v))
        }),
        ("PATCH", _) => {
            if let Some(id) = resource(action, "devices") {
                decode::<DeviceRename>(bytes)
                    .map(|v| PermissionMutation::RenameDevice(id.into(), v))
            } else {
                resource(action, "roles").and_then(|id| {
                    decode::<RolePatch>(bytes).map(|v| PermissionMutation::PatchRole(id.into(), v))
                })
            }
        }
        ("DELETE", _) => {
            if let Some(id) = resource(action, "roles") {
                decode::<DeleteRole>(bytes)
                    .map(|v| PermissionMutation::DeleteRole(id.into(), v.expected_version))
            } else if bytes.is_empty() {
                resource(action, "devices")
                    .map(|id| PermissionMutation::RevokeDevice(id.into()))
                    .or_else(|| {
                        resource(action, "invitations")
                            .map(|id| PermissionMutation::RevokeInvitation(id.into()))
                    })
            } else {
                None
            }
        }
        _ => None,
    };
    let Some(command) = command else {
        return error(StatusCode::BAD_REQUEST, "invalid_request");
    };
    let Some(key) = idempotency else {
        return error(StatusCode::BAD_REQUEST, "invalid_request");
    };
    match host.management().permission_mutate(token, key, command) {
        Ok(mut v) => {
            if action == "invitations"
                && method == Method::POST
                && let Some(code) = v.get("code").and_then(|v| v.as_str())
            {
                let endpoint = match host.configuration().listener() {
                    vesta_control::config::ListenerDescription::Tls(listener) => {
                        listener.public_endpoint()
                    }
                    _ => None,
                };
                let link = vesta_control::identity::management::pairing_link(
                    endpoint,
                    request.tls_fingerprint,
                    code,
                    host.management().server_id(),
                );
                v["pairing_link"] = serde_json::json!(link);
            }
            ok(if action == "server" {
                server(host, v)
            } else {
                v
            })
        }
        Err(e) => failure(e),
    }
}

fn server(host: &NetworkHost, mut value: serde_json::Value) -> serde_json::Value {
    let pack = host.configuration().pack();
    if value["name"].is_null() {
        value["name"] = serde_json::json!(pack.name());
    }
    if value["description"].is_null() {
        value["description"] = serde_json::json!(pack.description());
    }
    value["server_id"] = serde_json::json!(host.management().server_id());
    value["agent_version"] = serde_json::json!(env!("CARGO_PKG_VERSION"));
    value["minecraft_version"] = serde_json::json!(pack.minecraft_version());
    value["pack_version"] = serde_json::json!(pack.version());
    value["backups_configured"] = serde_json::json!(host.backups().is_some());
    value["join_address"] = serde_json::json!(pack.join().map(|join| {
        if join.address().contains(':') {
            format!("[{}]:{}", join.address(), join.port())
        } else {
            format!("{}:{}", join.address(), join.port())
        }
    }));
    value["game_port"] = serde_json::json!(host.game_port());
    value["configured_mode"] = serde_json::json!("same_account");
    value
}
fn strong_etag(value: &str) -> bool {
    value.len() == 66
        && value.starts_with('"')
        && value.ends_with('"')
        && value.as_bytes()[1..65]
            .iter()
            .all(|v| matches!(v,b'0'..=b'9'|b'a'..=b'f'))
}
fn icon(host: &NetworkHost, request: Request<'_>, token: &str) -> Reply {
    if request.query.is_some() || request.range.is_some() || request.if_range.is_some() {
        return error(StatusCode::BAD_REQUEST, "invalid_request");
    }
    if request.bytes.len() > MAX_ICON_BYTES {
        return error(StatusCode::PAYLOAD_TOO_LARGE, "request_too_large");
    }
    if request.method == Method::GET {
        if !request.bytes.is_empty()
            || request.if_match.is_some()
            || request.if_none_match.is_some()
        {
            return error(StatusCode::BAD_REQUEST, "invalid_request");
        }
        return match host.management().branding_icon(token) {
            Ok(Some(bytes)) => {
                let etag = icon_etag(&bytes);
                let mut reply = super::response(StatusCode::OK, bytes, "image/png");
                reply
                    .headers_mut()
                    .insert("etag", etag.parse().expect("hex ETag"));
                reply
            }
            Ok(None) => error(StatusCode::NOT_FOUND, "resource_not_found"),
            Err(e) => failure(e),
        };
    }
    let Some(key) = request.idempotency else {
        return error(StatusCode::BAD_REQUEST, "invalid_request");
    };
    let command = match request.method.as_str() {
        "PUT" => {
            let expected = match (request.if_match, request.if_none_match) {
                (Some(value), None) if strong_etag(value) => Some(value.to_owned()),
                (None, Some("*")) => None,
                _ => return error(StatusCode::BAD_REQUEST, "invalid_request"),
            };
            PermissionMutation::PutIcon(expected, request.bytes.to_vec())
        }
        "DELETE" if request.bytes.is_empty() && request.if_none_match.is_none() => {
            let Some(expected) = request.if_match.filter(|v| strong_etag(v)) else {
                return error(StatusCode::BAD_REQUEST, "invalid_request");
            };
            PermissionMutation::DeleteIcon(expected.to_owned())
        }
        _ => return error(StatusCode::BAD_REQUEST, "invalid_request"),
    };
    match host.management().permission_mutate(token, key, command) {
        Ok(value) => {
            let etag = value
                .get("etag")
                .and_then(|v| v.as_str())
                .map(str::to_owned);
            let mut reply = ok(value);
            if let Some(etag) = etag {
                reply
                    .headers_mut()
                    .insert("etag", etag.parse().expect("hex ETag"));
            }
            reply
        }
        Err(e) => failure(e),
    }
}
