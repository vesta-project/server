//! Owner-reviewed hosted Minecraft packs. Client-modpack endpoints are separate.
use super::{Request, decode, error, key, ok};
use crate::Reply;
use http::{Method, StatusCode};
use serde::Deserialize;
use vesta_control::{host::NetworkHost, identity::Subject, resource_packs::hosting};
pub(super) fn implemented(path: &str) -> bool {
    matches!(
        path,
        "/api/v3/manage/resource-packs"
            | "/api/v3/manage/resource-packs/recovery/abandon"
            | "/api/v3/manage/resource-packs/draft"
            | "/api/v3/manage/resource-packs/build"
            | "/api/v3/manage/resource-packs/plan"
            | "/api/v3/manage/resource-packs/apply"
            | "/api/v3/manage/resource-packs/uploads/begin"
            | "/api/v3/manage/resource-packs/uploads/chunk"
            | "/api/v3/manage/resource-packs/uploads/finish"
            | "/api/v3/manage/resource-packs/uploads/cancel"
    )
}
pub(super) fn failure(e: hosting::Error) -> Reply {
    use hosting::Error::*;
    let status = match e {
        Forbidden => StatusCode::FORBIDDEN,
        NotFound => StatusCode::NOT_FOUND,
        Invalid => StatusCode::BAD_REQUEST,
        Conflict => StatusCode::PRECONDITION_FAILED,
        IdempotencyConflict | Busy | Confirmation | StoppedRequired | RecoveryRequired
        | TargetChanged => StatusCode::CONFLICT,
        Limit => StatusCode::INSUFFICIENT_STORAGE,
        _ => StatusCode::SERVICE_UNAVAILABLE,
    };
    error(status, e.code())
}
fn value<T: serde::Serialize>(result: hosting::Result<T>) -> Reply {
    match result {
        Ok(v) => match serde_json::to_value(v) {
            Ok(v) => ok(v),
            Err(_) => error(StatusCode::SERVICE_UNAVAILABLE, "resource_pack_unavailable"),
        },
        Err(e) => failure(e),
    }
}
pub(super) fn dispatch(host: &NetworkHost, actor: &Subject, request: Request<'_>) -> Reply {
    let service = host.resource_packs();
    if request.path == "/api/v3/manage/resource-packs" && request.method == Method::GET {
        return value(service.status(actor));
    }
    if request.method != Method::POST {
        return error(StatusCode::METHOD_NOT_ALLOWED, "method_not_allowed");
    }
    let Some(key) = key(
        request.idempotency,
        host.database().server_id(),
        actor.device.as_str(),
        request.path,
    ) else {
        return error(StatusCode::BAD_REQUEST, "idempotency_key_required");
    };
    macro_rules! call {
        ($ty:ty,$method:ident) => {{
            let Some(input) = decode::<$ty>(request.bytes) else {
                return error(StatusCode::BAD_REQUEST, "invalid_request");
            };
            value(service.$method(actor, &key, &input))
        }};
    }
    match request.path {
        "/api/v3/manage/resource-packs/recovery/abandon" => {
            call!(hosting::AbandonRecovery, abandon_recovery)
        }
        "/api/v3/manage/resource-packs/draft" => call!(hosting::Save, save),
        "/api/v3/manage/resource-packs/build" => call!(hosting::BuildRequest, build),
        "/api/v3/manage/resource-packs/apply" => call!(hosting::Apply, apply),
        "/api/v3/manage/resource-packs/uploads/begin" => call!(hosting::UploadBegin, upload_begin),
        "/api/v3/manage/resource-packs/uploads/chunk" => call!(hosting::UploadChunk, upload_chunk),
        "/api/v3/manage/resource-packs/plan" => {
            let Some(input) = decode::<hosting::PlanRequest>(request.bytes) else {
                return error(StatusCode::BAD_REQUEST, "invalid_request");
            };
            value(service.plan(actor, &input))
        }
        "/api/v3/manage/resource-packs/uploads/finish"
        | "/api/v3/manage/resource-packs/uploads/cancel" => {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct UploadId {
                upload_id: String,
            }
            let Some(input) = decode::<UploadId>(request.bytes) else {
                return error(StatusCode::BAD_REQUEST, "invalid_request");
            };
            if request.path.ends_with("finish") {
                value(service.upload_finish(actor, &key, &input.upload_id))
            } else {
                value(service.upload_cancel(actor, &key, &input.upload_id))
            }
        }
        _ => error(StatusCode::NOT_FOUND, "endpoint_not_found"),
    }
}
