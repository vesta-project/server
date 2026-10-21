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
    },
};

pub(super) fn implemented(path: &str) -> bool {
    let Some(action) = path.strip_prefix("/api/v3/manage/") else {
        return false;
    };
    matches!(
        action,
        "users" | "roles" | "access-sources" | "invitations" | "devices"
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
            Ok(v) => ok(v),
            Err(e) => failure(e),
        };
    }
    if query.is_some() {
        return error(StatusCode::BAD_REQUEST, "invalid_request");
    }
    let command = match (method.as_str(), action) {
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
            if action == "invitations" && method == Method::POST {
                if let Some(code) = v.get("code").and_then(|v| v.as_str()) {
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
            }
            ok(v)
        }
        Err(e) => failure(e),
    }
}
