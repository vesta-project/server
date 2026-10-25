//! Owner hosting operations; fixed routes and bounded request shapes only.
use super::{Request, decode, envelope, error, management_error, ok};
use crate::Reply;
use http::{Method, StatusCode};
use serde::Deserialize;
use serde_json::json;
use vesta_control::{
    files::FileError,
    host::NetworkHost,
    identity::Capability,
    software::{ApplyRequest, PlanRequest, RollbackRequest, SoftwareError},
};

pub(crate) fn implemented(path: &str) -> bool {
    matches!(
        path,
        "/api/v3/manage/world"
            | "/api/v3/manage/world/new"
            | "/api/v3/manage/world/datapacks/toggle"
            | "/api/v3/manage/java"
            | "/api/v3/manage/java/select"
            | "/api/v3/manage/java/download"
            | "/api/v3/manage/software"
            | "/api/v3/manage/software/versions"
            | "/api/v3/manage/software/releases"
            | "/api/v3/manage/software/plan"
            | "/api/v3/manage/software/apply"
            | "/api/v3/manage/software/jobs"
            | "/api/v3/manage/software/rollback/plan"
            | "/api/v3/manage/software/rollback"
            | "/api/v3/manage/sftp"
            | "/api/v3/manage/sftp/keys"
    ) || path
        .strip_prefix("/api/v3/manage/sftp/keys/")
        .is_some_and(|id| !id.is_empty() && !id.contains('/'))
}
fn software_error(error_value: SoftwareError) -> Reply {
    use SoftwareError::*;
    let status = match error_value {
        Forbidden => StatusCode::FORBIDDEN,
        InvalidRequest => StatusCode::BAD_REQUEST,
        RevisionConflict => StatusCode::PRECONDITION_FAILED,
        Busy | PlanExpired | ConfirmationInvalid | IdempotencyConflict | PreviousUnavailable
        | RecoveryRequired => StatusCode::CONFLICT,
        _ => StatusCode::SERVICE_UNAVAILABLE,
    };
    error(status, error_value.code())
}
fn world_error(value: FileError) -> Reply {
    match value {
        FileError::Conflict(_) => error(StatusCode::PRECONDITION_FAILED, "world_state_conflict"),
        FileError::Corrupt => error(StatusCode::UNPROCESSABLE_ENTITY, "world_metadata_invalid"),
        other => super::files::file_error(other),
    }
}
fn json_value<T: serde::Serialize>(value: T) -> Reply {
    match serde_json::to_value(value) {
        Ok(value) => ok(value),
        Err(_) => error(StatusCode::SERVICE_UNAVAILABLE, "temporarily_unavailable"),
    }
}
pub(super) fn dispatch(host: &NetworkHost, request: Request<'_>) -> Reply {
    if request.if_match.is_some()
        || request.if_none_match.is_some()
        || request.range.is_some()
        || request.if_range.is_some()
    {
        return error(StatusCode::BAD_REQUEST, "invalid_request");
    }
    let Some(token) = request.authorization else {
        return error(StatusCode::UNAUTHORIZED, "management_session_invalid");
    };
    let subject = match host.management().authenticate(token) {
        Ok(subject) => subject,
        Err(e) => return management_error(e),
    };
    let action = request
        .path
        .strip_prefix("/api/v3/manage/")
        .unwrap_or_default();
    if !matches!(action, "software/versions" | "software/releases") && request.query.is_some() {
        return error(StatusCode::BAD_REQUEST, "invalid_request");
    }
    if action == "world" && request.method == Method::GET {
        return match host.files().world(&subject) {
            Ok(world) => json_value(world),
            Err(e) => world_error(e),
        };
    }
    if action == "world/new" && request.method == Method::POST {
        let Some(key) = super::key(
            request.idempotency,
            host.database().server_id(),
            subject.device.as_str(),
            request.path,
        ) else {
            return error(StatusCode::BAD_REQUEST, "idempotency_key_required");
        };
        let Some(patch) = decode(request.bytes) else {
            return error(StatusCode::BAD_REQUEST, "invalid_request");
        };
        return match host
            .files()
            .new_world(host.runtime(), &subject, &key, patch)
        {
            Ok(value) => ok(value),
            Err(e) => world_error(e),
        };
    }
    if action == "world/datapacks/toggle" {
        if request.method != Method::POST || request.query.is_some() {
            return error(StatusCode::BAD_REQUEST, "invalid_request");
        }
        if let Err(e) = host
            .management()
            .authorize(token, Capability::ConfigEdit, None)
        {
            return management_error(e);
        }
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Toggle {
            pack_id: String,
            enabled: bool,
            expected_generation: String,
        }
        let Some(input) = decode::<Toggle>(request.bytes) else {
            return error(StatusCode::BAD_REQUEST, "invalid_request");
        };
        let Ok(generation) =
            vesta_control::operations::GenerationId::parse_hex(&input.expected_generation)
        else {
            return error(StatusCode::BAD_REQUEST, "invalid_request");
        };
        if host
            .runtime()
            .game_snapshot()
            .ok()
            .and_then(|s| s.generation)
            != Some(generation)
        {
            return error(StatusCode::CONFLICT, "datapack_requires_running_game");
        }
        let world = match host.files().world(&subject) {
            Ok(w) => w,
            Err(e) => return world_error(e),
        };
        if !world
            .details
            .datapacks
            .iter()
            .any(|p| p.id == input.pack_id && p.kind != "missing")
            || input.pack_id.chars().any(char::is_control)
        {
            return error(StatusCode::BAD_REQUEST, "invalid_request");
        }
        let Some(key) = super::key(
            request.idempotency,
            host.management().server_id(),
            subject.device.as_str(),
            request.path,
        ) else {
            return error(StatusCode::BAD_REQUEST, "idempotency_key_required");
        };
        let line = format!(
            "datapack {} {}",
            if input.enabled { "enable" } else { "disable" },
            serde_json::to_string(&input.pack_id).expect("string")
        );
        return match host.runtime().submit_game(
            &subject,
            vesta_control::operations::GameCommand::Console {
                expected_generation: generation,
                line,
            },
            &key,
        ) {
            Ok(
                vesta_control::operations::Admission::Created(op)
                | vesta_control::operations::Admission::Existing(op),
            ) => ok(super::summary(&op)),
            Err(e) => super::operation_error(e),
        };
    }
    if action.starts_with("sftp") {
        let service = host.sftp();
        let mutation_cap = match (action, request.method) {
            ("sftp", &Method::PATCH) => Some(Capability::SecurityManage),
            ("sftp/keys", &Method::POST) => Some(Capability::CredentialsManage),
            (_, &Method::DELETE) if action.starts_with("sftp/keys/") => {
                Some(Capability::CredentialsManage)
            }
            _ => None,
        };
        let mutation_key = if let Some(cap) = mutation_cap {
            let Some(key) = super::key(
                request.idempotency,
                host.database().server_id(),
                subject.device.as_str(),
                request.path,
            ) else {
                return error(StatusCode::BAD_REQUEST, "idempotency_key_required");
            };
            match service.begin_mutation(token, cap, &key, action, request.bytes) {
                Ok(Some(result)) => return sftp_result(result),
                Ok(None) => Some(key),
                Err(e) => return sftp_result(Err(e)),
            }
        } else {
            None
        };
        let result = match (action, request.method) {
            ("sftp", &Method::GET) => service.status(token),
            ("sftp", &Method::PATCH) => {
                let Some(patch) = decode(request.bytes) else {
                    return error(StatusCode::BAD_REQUEST, "invalid_request");
                };
                let Some(owner) = service.owner_runtime() else {
                    return error(StatusCode::SERVICE_UNAVAILABLE, "sftp_unavailable");
                };
                let service = service.clone();
                let token = token.to_owned();
                return match owner.block_on(owner.spawn(async move {
                    let result = service.patch(&token, patch).await;
                    if let Some(key) = mutation_key {
                        service.finish_mutation(&key, &result)?;
                    }
                    result
                })) {
                    Ok(result) => sftp_result(result),
                    Err(_) => error(StatusCode::SERVICE_UNAVAILABLE, "sftp_outcome_unknown"),
                };
            }
            ("sftp/keys", &Method::GET) => service.keys(token),
            ("sftp/keys", &Method::POST) => {
                let Some(input) = decode(request.bytes) else {
                    return error(StatusCode::BAD_REQUEST, "invalid_request");
                };
                service.register_key(token, input)
            }
            (_, &Method::DELETE)
                if action.starts_with("sftp/keys/") && request.bytes.is_empty() =>
            {
                service.revoke_key(token, action.strip_prefix("sftp/keys/").unwrap_or_default())
            }
            _ => return error(StatusCode::BAD_REQUEST, "invalid_request"),
        };
        if let Some(key) = mutation_key
            && let Err(e) = service.finish_mutation(&key, &result)
        {
            return sftp_result(Err(e));
        }
        return sftp_result(result);
    }
    let Some(service) = host.software() else {
        if let Err(e) = host
            .management()
            .authorize(token, Capability::ContentInstall, None)
        {
            return management_error(e);
        }
        return if action == "software" && request.method == Method::GET {
            ok(
                json!({"available":false,"unavailable_reason":"software_runtime_unavailable","revision":0,"current":null,"previous":null,"recovery_required":false,"catalog":[]}),
            )
        } else {
            error(
                StatusCode::SERVICE_UNAVAILABLE,
                "software_runtime_unavailable",
            )
        };
    };
    if action.starts_with("java") {
        if let Err(e) = vesta_control::java::authorize(host.database(), &subject) {
            return software_error(e);
        }
        let selected = match service.selected_java_path(&subject) {
            Ok(p) => p,
            Err(e) => return software_error(e),
        };
        if action == "java" && request.method == Method::GET {
            let config = host.configuration().clone();
            return match run_async(host, async move {
                vesta_control::java::inventory(&config, &selected).await
            }) {
                Ok(value) => json_value(value),
                Err(e) => software_error(e),
            };
        }
        if request.method != Method::POST || request.query.is_some() {
            return error(StatusCode::BAD_REQUEST, "invalid_request");
        }
        let Some(key) = request.idempotency else {
            return error(StatusCode::BAD_REQUEST, "idempotency_key_required");
        };
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Select {
            runtime_id: Option<String>,
            custom_path: Option<String>,
        }
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Download {
            major: u32,
        }
        let result = if action == "java/select" {
            let Some(input) = decode::<Select>(request.bytes) else {
                return error(StatusCode::BAD_REQUEST, "invalid_request");
            };
            let config = host.configuration().clone();
            match run_async(host, async move {
                vesta_control::java::select(
                    &config,
                    &selected,
                    input.runtime_id.as_deref(),
                    input.custom_path.as_deref(),
                )
                .await
            }) {
                Ok(runtime) => service.java_job(
                    &subject,
                    Some(std::path::PathBuf::from(runtime.path)),
                    None,
                    key,
                ),
                Err(e) => Err(e),
            }
        } else if action == "java/download" {
            let Some(input) =
                decode::<Download>(request.bytes).filter(|d| (8..=99).contains(&d.major))
            else {
                return error(StatusCode::BAD_REQUEST, "invalid_request");
            };
            service.java_job(
                &subject,
                None,
                Some((host.configuration().clone(), input.major)),
                key,
            )
        } else {
            return error(StatusCode::BAD_REQUEST, "invalid_request");
        };
        return match result {
            Ok(job) => envelope(StatusCode::ACCEPTED, json!({"data":job})),
            Err(e) => software_error(e),
        };
    }
    match (action, request.method) {
        ("software", &Method::GET) => match service.status(&subject) {
            Ok(value) => json_value(value),
            Err(e) => software_error(e),
        },
        ("software/jobs", &Method::GET) => match service.jobs(&subject) {
            Ok(value) => ok(json!({"jobs":value})),
            Err(e) => software_error(e),
        },
        ("software/versions", &Method::GET) => {
            let Some(software) = request
                .query
                .and_then(|q| q.strip_prefix("software="))
                .filter(|q| !q.contains('&'))
                .and_then(|s| {
                    serde_json::from_value::<vesta_server_install::Software>(json!(s)).ok()
                })
            else {
                return error(StatusCode::BAD_REQUEST, "invalid_request");
            };
            let service = service.clone();
            let actor = subject.clone();
            match run_async(
                host,
                async move { service.versions(&actor, software).await },
            ) {
                Ok(value) => ok(json!({"software":software,"versions":value})),
                Err(e) => software_error(e),
            }
        }
        ("software/releases", &Method::GET) => {
            let Some((software, version)) = release_query(request.query) else {
                return error(StatusCode::BAD_REQUEST, "invalid_request");
            };
            let service = service.clone();
            let actor = subject.clone();
            let query_version = version.clone();
            match run_async(host, async move {
                service.releases(&actor, software, &query_version).await
            }) {
                Ok(value) => ok(json!({"software":software,"version":version,"releases":value})),
                Err(e) => software_error(e),
            }
        }
        ("software/plan", &Method::POST) => {
            let Some(input) = decode::<PlanRequest>(request.bytes) else {
                return error(StatusCode::BAD_REQUEST, "invalid_request");
            };
            let service = service.clone();
            let actor = subject.clone();
            match run_async(host, async move { service.plan(&actor, input).await }) {
                Ok(value) => json_value(value),
                Err(e) => software_error(e),
            }
        }
        ("software/apply", &Method::POST) => {
            let Some(input) = decode::<ApplyRequest>(request.bytes) else {
                return error(StatusCode::BAD_REQUEST, "invalid_request");
            };
            match service.apply(&subject, input) {
                Ok(value) => envelope(StatusCode::ACCEPTED, json!({"data":value})),
                Err(e) => software_error(e),
            }
        }
        ("software/rollback/plan", &Method::POST) => {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct Input {
                expected_revision: u64,
            }
            let Some(input) = decode::<Input>(request.bytes) else {
                return error(StatusCode::BAD_REQUEST, "invalid_request");
            };
            match service.prepare_rollback(&subject, input.expected_revision) {
                Ok(value) => json_value(value),
                Err(e) => software_error(e),
            }
        }
        ("software/rollback", &Method::POST) => {
            let Some(input) = decode::<RollbackRequest>(request.bytes) else {
                return error(StatusCode::BAD_REQUEST, "invalid_request");
            };
            match service.rollback(&subject, input) {
                Ok(value) => envelope(StatusCode::ACCEPTED, json!({"data":value})),
                Err(e) => software_error(e),
            }
        }
        _ => error(StatusCode::BAD_REQUEST, "invalid_request"),
    }
}
fn sftp_result(result: vesta_control::sftp::Result<serde_json::Value>) -> Reply {
    match result {
        Ok(value) => ok(value),
        Err(e) => error(
            if e.code() == "sftp_forbidden" {
                StatusCode::FORBIDDEN
            } else {
                StatusCode::CONFLICT
            },
            e.code(),
        ),
    }
}
fn run_async<T: Send + 'static>(
    host: &NetworkHost,
    future: impl std::future::Future<Output = vesta_control::software::Result<T>> + Send + 'static,
) -> vesta_control::software::Result<T> {
    let owner = host
        .sftp()
        .owner_runtime()
        .ok_or(SoftwareError::RuntimeUnavailable)?;
    owner
        .block_on(owner.spawn(future))
        .map_err(|_| SoftwareError::RuntimeUnavailable)?
}

// Fixed query grammar. Version identifiers never contain URL delimiters or escapes.
fn release_query(query: Option<&str>) -> Option<(vesta_server_install::Software, String)> {
    let (software, version) = query?.strip_prefix("software=")?.split_once("&version=")?;
    if version.is_empty()
        || version.len() > 128
        || !version
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
    {
        return None;
    }
    Some((
        serde_json::from_value(json!(software)).ok()?,
        version.into(),
    ))
}
#[cfg(test)]
mod release_query_tests {
    use super::*;
    #[test]
    fn rejects_ambiguous_or_unsafe_queries() {
        assert!(release_query(Some("software=paper&version=1.21.4")).is_some());
        for query in [
            None,
            Some("software=paper"),
            Some("software=paper&version=../x"),
            Some("software=paper&version=1&version=2"),
            Some("software=unknown&version=1"),
            Some("software=paper&version=%2F"),
        ] {
            assert!(release_query(query).is_none());
        }
    }
}
