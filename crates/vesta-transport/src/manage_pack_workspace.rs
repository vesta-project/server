//! Draft actions have independent pack capabilities and never touch live files.
use super::{Request, decode, error, management_error, ok, pack_error};
use crate::Reply;
use http::{Method, StatusCode};
use serde::Deserialize;
use vesta_control::{host::NetworkHost, identity::Capability, packs::workspace::*};
pub(super) fn implemented(path: &str) -> bool {
    matches!(
        path,
        "/api/v3/manage/pack/server-link"
            | "/api/v3/manage/pack/server-link/update"
            | "/api/v3/manage/pack/server-link/unlink"
            | "/api/v3/manage/pack/client-mode"
            | "/api/v3/manage/pack/history"
            | "/api/v3/manage/pack/draft"
            | "/api/v3/manage/pack/draft/add"
            | "/api/v3/manage/pack/draft/delete"
            | "/api/v3/manage/pack/draft/upload"
            | "/api/v3/manage/pack/draft/import"
            | "/api/v3/manage/pack/archive/inspect"
            | "/api/v3/manage/pack/install/plan"
            | "/api/v3/manage/pack/install/apply"
            | "/api/v3/manage/pack/provider/publish"
            | "/api/v3/manage/pack/files/read"
            | "/api/v3/manage/pack/files/mutate"
            | "/api/v3/manage/pack/suggestions"
            | "/api/v3/manage/pack/preview"
            | "/api/v3/manage/pack/draft/icon"
            | "/api/v3/manage/pack/draft/uploads/begin"
            | "/api/v3/manage/pack/draft/uploads/chunk"
            | "/api/v3/manage/pack/draft/uploads/finish"
            | "/api/v3/manage/pack/draft/uploads/cancel"
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
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UploadId {
    upload_id: String,
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
    if action == "pack/server-link" && request.method == Method::GET {
        if !request.bytes.is_empty() {
            return error(StatusCode::BAD_REQUEST, "invalid_request");
        }
        return value(workspace.server_link(&subject));
    }
    if action == "pack/server-link/update" && request.method == Method::GET {
        if !request.bytes.is_empty() {
            return error(StatusCode::BAD_REQUEST, "invalid_request");
        }
        let family = match host.software().map(|s| s.content_target()).transpose() {
            Ok(v) => v.unwrap_or_default().0,
            Err(_) => {
                return error(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "software_state_unavailable",
                );
            }
        };
        return value(workspace.server_modpack_update(
            &subject,
            &mc,
            family.as_deref(),
            &vesta_control::content::UreqProviderClient::configured(),
        ));
    }
    if action == "pack/history" && request.method == Method::GET {
        if !request.bytes.is_empty() {
            return error(StatusCode::BAD_REQUEST, "invalid_request");
        }
        return value(workspace.history(&subject));
    }
    if action == "pack/draft/icon" && request.method == Method::GET {
        if !request.bytes.is_empty() {
            return error(StatusCode::BAD_REQUEST, "invalid_request");
        }
        return value(workspace.icon(&subject, &mc));
    }
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
        let providers = vesta_control::content::UreqProviderClient::configured();
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
    if matches!(action, "pack/preview" | "pack/publish") {
        for cap in if action == "pack/publish" {
            vec![Capability::PackBuild, Capability::PackPublish]
        } else {
            vec![Capability::PackBuild]
        } {
            if let Err(e) = host.management().authorize(token, cap, None) {
                return management_error(e);
            }
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
        let providers = vesta_control::content::UreqProviderClient::configured();
        let suggestions = match workspace.server_mismatches(
            &subject,
            &mc,
            &inventory.items,
            inventory.truncated,
            &providers,
        ) {
            Ok(v) => v,
            Err(e) => return pack_error(e),
        };
        if action == "pack/preview" {
            let Some(input) = decode::<PreviewRequest>(request.bytes) else {
                return error(StatusCode::BAD_REQUEST, "invalid_request");
            };
            return value(workspace.preview(
                &subject,
                &mc,
                &input,
                host.releases(),
                &providers,
                suggestions,
            ));
        }
        let Some(key) = super::key(
            request.idempotency,
            host.database().server_id(),
            subject.device.as_str(),
            request.path,
        ) else {
            return error(StatusCode::BAD_REQUEST, "idempotency_key_required");
        };
        let Some(input) = decode::<WorkspacePublish>(request.bytes) else {
            return error(StatusCode::BAD_REQUEST, "invalid_request");
        };
        return match workspace.publish(
            &subject,
            &mc,
            &key,
            &input,
            host.releases(),
            &providers,
            &vesta_control::content::UreqDownloadHttp::default(),
            host.configuration().pack(),
            suggestions,
        ) {
            Ok(v) => super::pack_outcome(v),
            Err(e) => pack_error(e),
        };
    }
    if matches!(action, "pack/install/plan" | "pack/install/apply") {
        let (family, _) = match host.software().map(|s| s.content_target()).transpose() {
            Ok(v) => v.unwrap_or_default(),
            Err(_) => {
                return error(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "software_state_unavailable",
                );
            }
        };
        let providers = vesta_control::content::UreqProviderClient::configured();
        if action == "pack/install/plan" {
            let Some(input) = decode::<PackInstallRequest>(request.bytes) else {
                return error(StatusCode::BAD_REQUEST, "invalid_request");
            };
            return value(workspace.install_plan(
                &subject,
                &mc,
                family.as_deref(),
                &input,
                host.files(),
                host.releases(),
                &providers,
            ));
        }
        let Some(input) = decode::<PackInstallApply>(request.bytes) else {
            return error(StatusCode::BAD_REQUEST, "invalid_request");
        };
        let Some(key) = super::key(
            request.idempotency,
            host.database().server_id(),
            subject.device.as_str(),
            request.path,
        ) else {
            return error(StatusCode::BAD_REQUEST, "idempotency_key_required");
        };
        if input.acknowledge_software_switch {
            let Some(service) = host.software().cloned() else {
                return error(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "software_state_unavailable",
                );
            };
            let pack = vesta_control::software::PackWork {
                workspace: workspace.clone(),
                files: host.files().clone(),
                input,
                key,
            };
            let actor = subject.clone();
            return match super::hosting::run_async(host, async move {
                service.apply_pack(&actor, pack).await
            }) {
                Ok(job) => ok(
                    serde_json::json!({"state":"queued", "pending_ids":[], "software_job_id":job.id, "modpack_revision":null}),
                ),
                Err(e) => super::hosting::software_error(e),
            };
        }
        let outcome = match workspace.install_apply(
            &subject,
            &mc,
            family.as_deref(),
            &key,
            &input,
            host.files(),
        ) {
            Ok(o) => o,
            Err(e) => return pack_error(e),
        };
        if let Err(e) = workspace.install_finish(&subject, &input.plan_id) {
            return pack_error(e);
        }
        return ok(
            serde_json::json!({"state":outcome.state,"pending_ids":outcome.pending_ids,"modpack_revision":null,"server_modpack":workspace.server_link(&subject).ok().flatten()}),
        );
    }
    if action == "pack/files/read" {
        let Some(input) = decode::<FileRead>(request.bytes) else {
            return error(StatusCode::BAD_REQUEST, "invalid_request");
        };
        return value(workspace.file_read(&subject, &mc, &input));
    }
    if action == "pack/archive/inspect" {
        let Some(input) = decode::<InspectRequest>(request.bytes) else {
            return error(StatusCode::BAD_REQUEST, "invalid_request");
        };
        let providers = vesta_control::content::UreqProviderClient::configured();
        return value(workspace.inspect_archive(&subject, &mc, &input, &providers));
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
        "pack/client-mode" => {
            let Some(input) = decode::<ClientPackConfig>(request.bytes) else {
                return error(StatusCode::BAD_REQUEST, "invalid_request");
            };
            value(workspace.set_client_pack(&subject, &key, &input))
        }
        "pack/server-link/unlink" => {
            let Some(input) = decode::<ServerModpackUnlink>(request.bytes) else {
                return error(StatusCode::BAD_REQUEST, "invalid_request");
            };
            value(workspace.unlink_server_modpack(&subject, &key, &input, host.files()))
        }

        "pack/provider/publish" => {
            let Some(input) = decode::<ProviderPackPublish>(request.bytes) else {
                return error(StatusCode::BAD_REQUEST, "invalid_request");
            };
            match workspace.publish_provider_pack(&subject, &mc, &key, &input, host.releases()) {
                Ok(v) => super::pack_outcome(v),
                Err(e) => pack_error(e),
            }
        }

        "pack/files/mutate" => {
            let Some(input) = decode::<FileMutation>(request.bytes) else {
                return error(StatusCode::BAD_REQUEST, "invalid_request");
            };
            value(workspace.file_mutate(&subject, &mc, &key, &input))
        }

        "pack/draft/uploads/begin" => {
            let Some(input) = decode::<UploadBegin>(request.bytes) else {
                return error(StatusCode::BAD_REQUEST, "invalid_request");
            };
            value(workspace.upload_begin(&subject, &key, &input))
        }
        "pack/draft/uploads/chunk" => {
            let Some(input) = decode::<UploadChunk>(request.bytes) else {
                return error(StatusCode::BAD_REQUEST, "invalid_request");
            };
            value(workspace.upload_chunk(&subject, &key, &input))
        }
        "pack/draft/uploads/finish" => {
            let Some(input) = decode::<UploadId>(request.bytes) else {
                return error(StatusCode::BAD_REQUEST, "invalid_request");
            };
            value(workspace.upload_finish(&subject, &key, &input.upload_id))
        }
        "pack/draft/uploads/cancel" => {
            let Some(input) = decode::<UploadId>(request.bytes) else {
                return error(StatusCode::BAD_REQUEST, "invalid_request");
            };
            value(workspace.upload_cancel(&subject, &key, &input.upload_id))
        }
        "pack/draft" => {
            let Some(input) = decode::<DraftWrite>(request.bytes) else {
                return error(StatusCode::BAD_REQUEST, "invalid_request");
            };
            value(workspace.write(&subject, &mc, &key, &input))
        }
        "pack/draft/add" => {
            if let Some(input) = decode::<DraftAdd>(request.bytes) {
                value(workspace.add(&subject, &mc, &key, &input))
            } else if let Some(input) = decode::<ProviderAdd>(request.bytes) {
                let providers = vesta_control::content::UreqProviderClient::configured();
                value(workspace.add_provider(&subject, &mc, &key, &input, &providers))
            } else {
                error(StatusCode::BAD_REQUEST, "invalid_request")
            }
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
            let providers = vesta_control::content::UreqProviderClient::configured();
            value(workspace.import(&subject, &mc, &key, &input, host.releases(), &providers))
        }
        _ => error(StatusCode::BAD_REQUEST, "invalid_request"),
    }
}
