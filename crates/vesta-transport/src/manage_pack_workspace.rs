//! Draft actions have independent pack capabilities and never touch live files.
use super::{Request, decode, error, management_error, ok, pack_error};
use crate::Reply;
use http::{Method, StatusCode};
use serde::Deserialize;
use vesta_control::{host::NetworkHost, identity::Capability, packs::workspace::*};
pub(super) fn implemented(path: &str) -> bool {
    matches!(
        path,
        "/api/v3/manage/pack/draft"
            | "/api/v3/manage/pack/draft/add"
            | "/api/v3/manage/pack/draft/delete"
            | "/api/v3/manage/pack/draft/upload"
            | "/api/v3/manage/pack/draft/import"
            | "/api/v3/manage/pack/suggestions"
    )
}
fn value(result: Result<impl serde::Serialize>) -> Reply {
    match result {
        Ok(v) => match serde_json::to_value(v) {
            Ok(v) => ok(v),
            Err(_) => error(StatusCode::SERVICE_UNAVAILABLE, "temporarily_unavailable"),
        },
        Err(e) => pack_error(e),
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Expected {
    expected_revision: String,
}
pub(super) fn dispatch(host: &NetworkHost, request: Request<'_>) -> Reply {
    let Some(token) = request.authorization else {
        return error(StatusCode::UNAUTHORIZED, "management_session_invalid");
    };
    let subject = match host.management().authenticate(token) {
        Ok(s) => s,
        Err(e) => return management_error(e),
    };
    if let Err(e) = host
        .management()
        .authorize(token, Capability::PackEdit, None)
    {
        return management_error(e);
    }
    if request.query.is_some()
        || request.if_match.is_some()
        || request.if_none_match.is_some()
        || request.range.is_some()
        || request.if_range.is_some()
        || request.bytes.len() > 2 * 1024 * 1024
    {
        return error(StatusCode::BAD_REQUEST, "invalid_request");
    }
    let workspace = match PackWorkspace::open(host.database()) {
        Ok(w) => w,
        Err(e) => return pack_error(e),
    };
    let mc = match host.software().map(|s| s.content_target()).transpose() {
        Ok(v) => v
            .and_then(|v| v.1)
            .unwrap_or_else(|| host.configuration().pack().minecraft_version().into()),
        Err(_) => {
            return error(
                StatusCode::SERVICE_UNAVAILABLE,
                "software_state_unavailable",
            );
        }
    };
    let action = request
        .path
        .strip_prefix("/api/v3/manage/")
        .unwrap_or_default();
    if action == "pack/suggestions" && request.method == Method::GET {
        if !request.bytes.is_empty() {
            return error(StatusCode::BAD_REQUEST, "invalid_request");
        }
        let (family, _) = match host.software().map(|s| s.content_target()).transpose() {
            Ok(v) => v.unwrap_or_default(),
            Err(_) => {
                return error(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "software_state_unavailable",
                );
            }
        };
        let world = match host.files().world(&subject) {
            Ok(w) => w,
            Err(e) => return super::files::file_error(e),
        };
        let inventory = match host
            .files()
            .content_inventory(&subject, family, mc.clone(), &world)
        {
            Ok(v) => v,
            Err(e) => return super::files::file_error(e),
        };
        let providers = vesta_control::content::UreqProviderClient::new(
            std::env::var("VESTA_CURSEFORGE_API_KEY").ok(),
        );
        return value(workspace.suggestions(
            &subject,
            &mc,
            &inventory.items,
            inventory.truncated,
            &providers,
        ));
    }
    if action == "pack/draft" && request.method == Method::GET {
        if !request.bytes.is_empty() {
            return error(StatusCode::BAD_REQUEST, "invalid_request");
        }
        return value(workspace.view(&subject, &mc, host.releases()));
    }
    if request.method != Method::POST {
        return error(StatusCode::METHOD_NOT_ALLOWED, "method_not_allowed");
    }
    let Some(key) = super::key(
        request.idempotency,
        host.database().server_id(),
        subject.device.as_str(),
        request.path,
    ) else {
        return error(StatusCode::BAD_REQUEST, "idempotency_key_required");
    };
    match action {
        "pack/draft" => {
            let Some(input) = decode::<DraftWrite>(request.bytes) else {
                return error(StatusCode::BAD_REQUEST, "invalid_request");
            };
            value(workspace.write(&subject, &mc, &key, &input))
        }
        "pack/draft/add" => {
            let Some(input) = decode::<DraftAdd>(request.bytes) else {
                return error(StatusCode::BAD_REQUEST, "invalid_request");
            };
            value(workspace.add(&subject, &mc, &key, &input))
        }
        "pack/draft/delete" => {
            let Some(input) = decode::<Expected>(request.bytes) else {
                return error(StatusCode::BAD_REQUEST, "invalid_request");
            };
            value(workspace.delete(&subject, &mc, &key, &input.expected_revision))
        }
        "pack/draft/upload" => {
            let Some(input) = decode::<UploadRequest>(request.bytes) else {
                return error(StatusCode::BAD_REQUEST, "invalid_request");
            };
            value(workspace.upload(&subject, &key, &input))
        }
        "pack/draft/import" => {
            let Some(input) = decode::<ImportRequest>(request.bytes) else {
                return error(StatusCode::BAD_REQUEST, "invalid_request");
            };
            let providers = vesta_control::content::UreqProviderClient::new(
                std::env::var("VESTA_CURSEFORGE_API_KEY").ok(),
            );
            value(workspace.import(&subject, &mc, &key, &input, host.releases(), &providers))
        }
        _ => error(StatusCode::BAD_REQUEST, "invalid_request"),
    }
}
