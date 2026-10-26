//! Session-authenticated management projections and durable lifecycle admission.
use super::{Reply, response};
use http::{Method, StatusCode};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};
use vesta_control::{
    host::NetworkHost,
    identity::{Capability, management::ManagementError},
    operations::{
        ActivityActor, ActivitySummary, Admission, ConfirmationToken, GameCommand, GenerationId,
        OperationActivity, OperationError, OperationErrorCode, OperationId, OperationState,
        OperationSummary,
    },
};

#[path = "manage_automation.rs"]
mod automation;
#[path = "manage_content.rs"]
pub(super) mod content;
#[path = "manage_files.rs"]
pub(super) mod files;
#[path = "manage_hosting.rs"]
pub(super) mod hosting;
#[path = "manage_metrics.rs"]
mod metrics;
#[path = "manage_moderation.rs"]
mod moderation;
#[path = "manage_pack_workspace.rs"]
pub(super) mod pack_workspace;
#[path = "manage_permissions.rs"]
mod permissions;
#[path = "manage_presentation.rs"]
pub(super) mod presentation;
#[path = "manage_status.rs"]
pub(super) mod status;

static REQUEST: AtomicU64 = AtomicU64::new(1);
pub(super) fn permission_endpoint(path: &str) -> bool {
    permissions::implemented(path)
}
pub(super) fn file_endpoint(path: &str) -> bool {
    files::implemented(path)
}
pub(super) fn metrics_endpoint(path: &str) -> bool {
    metrics::implemented(path)
}
pub(super) fn automation_endpoint(path: &str) -> bool {
    automation::implemented(path)
}
pub(super) fn implemented(path: &str) -> bool {
    crate::pack_sync::implemented(path) || pack_workspace::implemented(path)
        || hosting::implemented(path)
        || automation::implemented(path)
        || presentation::implemented(path)
        || moderation::implemented(path)
        || metrics::implemented(path)
        || files::implemented(path)
        || permissions::implemented(path)
        || matches!(
            path,
            "/api/v3/manage/server"
                | "/api/v3/manage/server/status"
                | "/api/v3/manage/lifecycle/start"
                | "/api/v3/manage/lifecycle/sleep"
                | "/api/v3/manage/lifecycle/wake"
                | "/api/v3/manage/lifecycle/stop"
                | "/api/v3/manage/lifecycle/restart"
                | "/api/v3/manage/lifecycle/kill"
                | "/api/v3/manage/operations"
                | "/api/v3/manage/console"
                | "/api/v3/manage/console/stream"
                | "/api/v3/manage/players"
                | "/api/v3/manage/pack"
                | "/api/v3/manage/audit"
                | "/api/v3/manage/console/command"
                | "/api/v3/manage/pack/publish"
                | "/api/v3/manage/pack/minimum"
        )
        || path
            .strip_prefix("/api/v3/manage/operations/")
            .is_some_and(|id| !id.is_empty() && !id.contains('/'))
}
fn envelope(status: StatusCode, payload: Value) -> Reply {
    let mut value = json!({"api_version":"3", "request_id":format!("request-{}", REQUEST.fetch_add(1, Ordering::Relaxed))});
    value
        .as_object_mut()
        .expect("object")
        .extend(payload.as_object().expect("object").clone());
    response(
        status,
        serde_json::to_vec(&value).expect("JSON"),
        "application/json",
    )
}
pub(super) fn error(status: StatusCode, code: &'static str) -> Reply {
    envelope(status, json!({"error":{"code":code}}))
}
pub(super) fn ok(data: Value) -> Reply {
    envelope(StatusCode::OK, json!({"data": data}))
}
pub(super) fn management_error(e: ManagementError) -> Reply {
    match e {
        ManagementError::Forbidden => error(StatusCode::FORBIDDEN, "management_forbidden"),
        ManagementError::SessionInvalid | ManagementError::PairInvalid => {
            error(StatusCode::UNAUTHORIZED, "management_session_invalid")
        }
        ManagementError::InvalidRequest => error(StatusCode::BAD_REQUEST, "invalid_request"),
        _ => error(StatusCode::SERVICE_UNAVAILABLE, "temporarily_unavailable"),
    }
}
fn operation_error(e: OperationError) -> Reply {
    match e {
        OperationError::Rejected {
            code: OperationErrorCode::Forbidden,
            ..
        } => error(StatusCode::FORBIDDEN, "management_forbidden"),
        OperationError::Rejected {
            code: OperationErrorCode::InvalidRequest,
            ..
        } => error(StatusCode::BAD_REQUEST, "invalid_request"),
        OperationError::Rejected {
            code: OperationErrorCode::IdempotencyConflict,
            ..
        } => error(StatusCode::CONFLICT, "idempotency_conflict"),
        OperationError::Rejected {
            code: OperationErrorCode::ConfirmationRejected,
            ..
        } => error(StatusCode::CONFLICT, "confirmation_invalid"),
        OperationError::Rejected {
            code:
                OperationErrorCode::SourceRevisionUnavailable | OperationErrorCode::BindingMismatch,
            ..
        } => error(StatusCode::PRECONDITION_FAILED, "precondition_failed"),
        OperationError::Rejected { .. } => error(StatusCode::CONFLICT, "operation_conflict"),
        _ => error(StatusCode::SERVICE_UNAVAILABLE, "temporarily_unavailable"),
    }
}
fn summary(v: &OperationSummary) -> Value {
    json!({"operation_id":v.id.as_str(), "state":v.state.as_str(), "progress_percent":v.progress_percent, "cancel_requested":v.cancel_requested, "created_at_ms":v.created_at_ms, "updated_at_ms":v.updated_at_ms, "finished_at_ms":v.finished_at_ms})
}
fn activity(
    host: &NetworkHost,
    value: &(OperationSummary, OperationActivity),
) -> Result<Value, ManagementError> {
    let mut details = value.1.clone();
    redact_actor(host, &mut details.actor)?;
    match &mut details.summary {
        Some(ActivitySummary::Console { command }) => *command = redact_text(host, command)?,
        Some(ActivitySummary::Moderation {
            reason: Some(reason),
            ..
        }) => *reason = redact_text(host, reason)?,
        _ => {}
    }
    let mut result = summary(&value.0);
    result.as_object_mut().expect("object").extend(
        serde_json::to_value(details)
            .expect("typed Activity")
            .as_object()
            .expect("object")
            .clone(),
    );
    Ok(result)
}
fn redact_actor(host: &NetworkHost, actor: &mut ActivityActor) -> Result<(), ManagementError> {
    if let ActivityActor::Principal {
        display_name,
        device_name,
        ..
    } = actor
    {
        *display_name = redact_text(host, display_name)?;
        *device_name = redact_text(host, device_name)?;
    }
    Ok(())
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Empty {}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Command {
    expected_generation: String,
    line: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Publish {
    expected_revision: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Minimum {
    expected_revision: String,
    minimum_revision: String,
}
fn revision(value: &str) -> Option<u64> {
    if value.is_empty()
        || value.len() > 16
        || !value.bytes().all(|v| v.is_ascii_digit())
        || (value.len() > 1 && value.starts_with('0'))
    {
        return None;
    }
    let value = value.parse::<u64>().ok()?;
    (value <= 9_007_199_254_740_991).then_some(value)
}
fn pack_error(error_value: vesta_control::packs::ReleaseError) -> Reply {
    if error_value.code() == "forbidden" {
        return error(StatusCode::FORBIDDEN, "management_forbidden");
    }
    match error_value {
        vesta_control::packs::ReleaseError::Conflict => {
            error(StatusCode::PRECONDITION_FAILED, "precondition_failed")
        }
        vesta_control::packs::ReleaseError::IdempotencyConflict => {
            error(StatusCode::CONFLICT, "idempotency_conflict")
        }
        vesta_control::packs::ReleaseError::Control(_) => {
            error(StatusCode::FORBIDDEN, "management_forbidden")
        }
        vesta_control::packs::ReleaseError::RevisionMismatch => {
            error(StatusCode::CONFLICT, "pack_release_revision_mismatch")
        }
        vesta_control::packs::ReleaseError::VersionNotIncreasing => {
            error(StatusCode::CONFLICT, "pack_release_version_not_increasing")
        }
        vesta_control::packs::ReleaseError::InvalidVersion => {
            error(StatusCode::BAD_REQUEST, "pack_release_version_invalid")
        }
        vesta_control::packs::ReleaseError::Pack(e) => match e {
            vesta_control::packs::PackError::TooLarge => {
                error(StatusCode::PAYLOAD_TOO_LARGE, "pack_too_large")
            }
            vesta_control::packs::PackError::UnresolvedFile => {
                error(StatusCode::CONFLICT, "pack_unresolved_file")
            }
            vesta_control::packs::PackError::InvalidPath
            | vesta_control::packs::PackError::Catalogue(_)
            | vesta_control::packs::PackError::InvalidArchive => {
                error(StatusCode::BAD_REQUEST, e.code())
            }
            _ => error(StatusCode::SERVICE_UNAVAILABLE, "temporarily_unavailable"),
        },
        _ => error(StatusCode::SERVICE_UNAVAILABLE, "temporarily_unavailable"),
    }
}
fn pack_outcome(value: vesta_control::packs::PackOutcome) -> Reply {
    ok(
        json!({"outcome":"succeeded","hosted_revision":value.hosted_revision.to_string(),"minimum_revision":value.minimum_revision.to_string()}),
    )
}
/// Replace only exact known configured absolute paths and retained agent-issued credentials.
pub(super) fn redact_console(host: &NetworkHost, text: &str) -> Result<String, ManagementError> {
    host.management()
        .redact_console_secret_edges(&redact_text(host, text)?)
}
/// Complete typed fields have no capture boundary fragments; preserve ordinary words.
fn redact_text(host: &NetworkHost, text: &str) -> Result<String, ManagementError> {
    let config = host.configuration();
    let mut paths: Vec<String> = config
        .host_protected_paths()
        .iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect();
    for path in [
        host.database().directory().path(),
        config.base(),
        config.source(),
        config.state(),
        config.launch_definition(),
    ] {
        paths.push(path.to_string_lossy().into_owned());
    }
    let mut variants = Vec::new();
    for path in paths {
        if !path.is_empty() {
            variants.push(path.replace('\\', "/"));
            variants.push(path.replace('/', "\\"));
        }
    }
    variants.sort_by_key(|v| std::cmp::Reverse(v.len()));
    variants.dedup();
    let mut text = host.management().redact_console_secrets(text)?;
    for path in variants {
        text = text.replace(&path, "[redacted]");
    }
    Ok(text)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Generation {
    expected_generation: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Kill {
    expected_generation: String,
    acknowledgement: String,
    #[serde(default)]
    confirmation_token: Option<String>,
}
fn decode<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Option<T> {
    if bytes.iter().copied().find(|b| !b.is_ascii_whitespace()) != Some(b'{') {
        return None;
    }
    serde_json::from_slice(bytes).ok()
}
fn key(value: Option<&str>, server: &str, device: &str, path: &str) -> Option<String> {
    let value = value?;
    if !(16..=128).contains(&value.len()) || !value.bytes().all(|b| b.is_ascii_graphic()) {
        return None;
    }
    use sha2::{Digest, Sha256};
    let mut hash = Sha256::new();
    for part in [server, device, "POST", path, value] {
        hash.update((part.len() as u64).to_be_bytes());
        hash.update(part.as_bytes());
    }
    Some(format!("http:{:x}", hash.finalize()))
}
fn page(query: Option<&str>, scope: &str, server: &str, maximum: usize) -> Option<(u64, usize)> {
    let mut cursor = None;
    let mut limit = None;
    if let Some(query) = query {
        if query.is_empty() || query.len() > 600 {
            return None;
        }
        for field in query.split('&') {
            let (name, value) = field.split_once('=')?;
            match name {
                "limit"
                    if limit.is_none()
                        && !value.is_empty()
                        && value.bytes().all(|b| b.is_ascii_digit()) =>
                {
                    limit = Some(value.parse::<usize>().ok()?)
                }
                "cursor" if cursor.is_none() && value.len() <= 512 => {
                    let prefix = format!("{server}.{scope}.");
                    let value = value.strip_prefix(&prefix)?;
                    if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
                        return None;
                    }
                    cursor = Some(value.parse::<u64>().ok()?);
                }
                _ => return None,
            }
        }
    }
    let limit = limit.unwrap_or(50);
    if !(1..=maximum).contains(&limit) {
        return None;
    }
    Some((cursor.unwrap_or(0), limit))
}
fn cursor(server: &str, scope: &str, value: impl std::fmt::Display) -> String {
    format!("{server}.{scope}.{value}")
}

pub(super) struct Request<'a> {
    pub tls_fingerprint: Option<&'a str>,
    pub method: &'a Method,
    pub path: &'a str,
    pub query: Option<&'a str>,
    pub authorization: Option<&'a str>,
    pub idempotency: Option<&'a str>,
    pub bytes: &'a [u8],
    pub if_match: Option<&'a str>,
    pub if_none_match: Option<&'a str>,
    pub range: Option<&'a str>,
    pub if_range: Option<&'a str>,
}
pub(super) fn dispatch(host: &NetworkHost, request: Request<'_>) -> Reply {
    if request.path == "/api/v3/manage/pack/publish"
        && serde_json::from_slice::<Value>(request.bytes)
            .is_ok_and(|v| v.get("draft_revision").is_some())
    {
        return pack_workspace::dispatch(host, request);
    }
    if pack_workspace::implemented(request.path) {
        return pack_workspace::dispatch(host, request);
    }
    if hosting::implemented(request.path) {
        return hosting::dispatch(host, request);
    }
    if request.path == "/api/v3/manage/server/status" {
        return status::dispatch(host, request);
    }

    if request.path == "/api/v3/manage/session" {
        if request.method != Method::GET || request.query.is_some() || !request.bytes.is_empty() {
            return error(StatusCode::BAD_REQUEST, "invalid_request");
        }
        let Some(token) = request.authorization else {
            return error(StatusCode::UNAUTHORIZED, "management_session_invalid");
        };
        return match host.with_policy(|policy| host.management().session_projection(token, policy))
        {
            Ok(Ok(value)) => ok(value),
            Ok(Err(e)) => management_error(e),
            Err(_) => error(StatusCode::SERVICE_UNAVAILABLE, "temporarily_unavailable"),
        };
    }

    if presentation::implemented(request.path) {
        return presentation::dispatch(host, request);
    }
    if moderation::implemented(request.path) {
        return moderation::dispatch(host, request);
    }
    if metrics::implemented(request.path) {
        return metrics::dispatch(host, request);
    }
    if automation::implemented(request.path) {
        return automation::dispatch(host, request);
    }
    if permissions::implemented(request.path) {
        return permissions::dispatch(host, request);
    }
    if files::implemented(request.path) {
        return files::dispatch(host, request);
    }
    let Request {
        method,
        path,
        query,
        authorization,
        idempotency,
        bytes,
        ..
    } = request;
    let action = path
        .strip_prefix("/api/v3/manage/")
        .expect("management path");
    let capability = match action {
        "server" | "server/status" | "pack" => Capability::StatusView,
        "lifecycle/start" | "lifecycle/wake" => Capability::LifecycleStart,
        "lifecycle/sleep" => Capability::LifecycleStop,
        "lifecycle/stop" => Capability::LifecycleStop,
        "lifecycle/restart" => Capability::LifecycleRestart,
        "lifecycle/kill" => Capability::LifecycleForceKill,
        "operations" => Capability::TasksView,
        "console" | "console/stream" => Capability::ConsoleRead,
        "players" => Capability::PlayersView,
        "audit" => Capability::AuditView,
        "console/command" => Capability::ConsoleWrite,
        "pack/publish" => Capability::PackPublish,
        "pack/minimum" => Capability::PackForceUpdate,
        _ if action.starts_with("operations/") => Capability::TasksView,
        _ => return error(StatusCode::NOT_IMPLEMENTED, "not_implemented"),
    };
    let Some(token) = authorization else {
        return error(StatusCode::UNAUTHORIZED, "management_session_invalid");
    };
    let subject = match host.management().authorize(token, capability, None) {
        Ok(s) => s,
        Err(e) => return management_error(e),
    };
    if action == "console/stream" {
        return error(StatusCode::NOT_IMPLEMENTED, "not_implemented");
    }
    let mutation = action.starts_with("lifecycle/")
        || matches!(action, "console/command" | "pack/publish" | "pack/minimum");
    if (mutation && method != Method::POST) || (!mutation && method != Method::GET) {
        return error(StatusCode::BAD_REQUEST, "invalid_request");
    }
    if mutation && query.is_some() {
        return error(StatusCode::BAD_REQUEST, "invalid_request");
    }
    let runtime = host.runtime();
    let operations = runtime.operations_service();
    if mutation {
        let Some(mut scoped_key) = key(
            idempotency,
            host.management().server_id(),
            subject.device.as_str(),
            path,
        ) else {
            return error(StatusCode::BAD_REQUEST, "invalid_request");
        };
        if matches!(action, "lifecycle/start" | "lifecycle/wake") {
            scoped_key = format!(
                "launcher-{}:{scoped_key}",
                if action.ends_with("/wake") {
                    "wake"
                } else {
                    "start"
                }
            );
        }
        if action.starts_with("pack/") {
            let (expected, minimum) = if action == "pack/publish" {
                let Some(input) = decode::<Publish>(bytes) else {
                    return error(StatusCode::BAD_REQUEST, "invalid_request");
                };
                let Some(expected) = revision(&input.expected_revision) else {
                    return error(StatusCode::BAD_REQUEST, "invalid_request");
                };
                (expected, None)
            } else {
                let Some(input) = decode::<Minimum>(bytes) else {
                    return error(StatusCode::BAD_REQUEST, "invalid_request");
                };
                let (Some(expected), Some(minimum)) = (
                    revision(&input.expected_revision),
                    revision(&input.minimum_revision),
                ) else {
                    return error(StatusCode::BAD_REQUEST, "invalid_request");
                };
                (expected, Some(minimum))
            };
            use vesta_control::packs::{PackMutation, UpdatePolicy};
            let request = match minimum {
                Some(minimum) => PackMutation::minimum(scoped_key, expected, minimum),
                None => PackMutation::publish(scoped_key, expected),
            };
            let request = match request {
                Ok(value) => value,
                Err(e) => return pack_error(e),
            };
            match host.releases().remote_outcome(&subject, &request) {
                Ok(Some(value)) => return pack_outcome(value),
                Ok(None) => (),
                Err(e) => return pack_error(e),
            }
            if let Some(minimum) = minimum {
                return match host
                    .releases()
                    .minimum_remote(&subject, expected, minimum, &request)
                {
                    Ok(value) => pack_outcome(value),
                    Err(e) => pack_error(e),
                };
            }
            let plan = match host
                .releases()
                .plan(&subject, expected, UpdatePolicy::Optional)
            {
                Ok(value) => value,
                Err(e) => {
                    return match host.releases().remote_outcome(&subject, &request) {
                        Ok(Some(value)) => pack_outcome(value),
                        _ => pack_error(e),
                    };
                }
            };
            let prepared = match host.prepare_pack(
                &plan,
                &vesta_protocol::pack::sources::SourceRegistry::default(),
            ) {
                Ok(value) => value,
                Err(_) => return error(StatusCode::SERVICE_UNAVAILABLE, "temporarily_unavailable"),
            };
            return match host
                .releases()
                .publish_remote(&subject, &plan, &prepared, &request)
            {
                Ok(value) => pack_outcome(value),
                Err(e) => match host.releases().remote_outcome(&subject, &request) {
                    Ok(Some(value)) => pack_outcome(value),
                    _ => pack_error(e),
                },
            };
        }
        let (command, confirmation) = match action {
            "console/command" => {
                let Some(input) = decode::<Command>(bytes) else {
                    return error(StatusCode::BAD_REQUEST, "invalid_request");
                };
                let Ok(expected_generation) = GenerationId::parse_hex(&input.expected_generation)
                else {
                    return error(StatusCode::BAD_REQUEST, "invalid_request");
                };
                (
                    GameCommand::Console {
                        expected_generation,
                        line: input.line,
                    },
                    None,
                )
            }
            "lifecycle/sleep" => {
                let Some(input) = decode::<Generation>(bytes) else {
                    return error(StatusCode::BAD_REQUEST, "invalid_request");
                };
                let Ok(expected_generation) = GenerationId::parse_hex(&input.expected_generation)
                else {
                    return error(StatusCode::BAD_REQUEST, "invalid_request");
                };
                if !host.sleep().available() {
                    return error(StatusCode::CONFLICT, "sleep_listener_not_configured");
                }
                (
                    GameCommand::ManualSleep {
                        expected_generation,
                    },
                    None,
                )
            }
            "lifecycle/stop" if decode::<Empty>(bytes).is_some() => {
                (GameCommand::StopSleeping, None)
            }
            "lifecycle/start" | "lifecycle/wake" => {
                if decode::<Empty>(bytes).is_none() {
                    return error(StatusCode::BAD_REQUEST, "invalid_request");
                }
                // Replays remain recoverable after the successful state transition.
                if !host
                    .sleep()
                    .has_lifecycle_intent(runtime, &subject, &scoped_key)
                    .unwrap_or(false)
                {
                    let sleeping = matches!(
                        host.sleep().public_state(runtime),
                        Ok(vesta_control::motd::State::Sleeping)
                    );
                    if (action == "lifecycle/wake") != sleeping {
                        return error(StatusCode::CONFLICT, "lifecycle_state_conflict");
                    }
                }
                (GameCommand::Start, None)
            }
            "lifecycle/stop" | "lifecycle/restart" => {
                let Some(input) = decode::<Generation>(bytes) else {
                    return error(StatusCode::BAD_REQUEST, "invalid_request");
                };
                let Ok(expected_generation) = GenerationId::parse_hex(&input.expected_generation)
                else {
                    return error(StatusCode::BAD_REQUEST, "invalid_request");
                };
                (
                    if action == "lifecycle/restart" {
                        GameCommand::Restart {
                            expected_generation,
                        }
                    } else {
                        GameCommand::Stop {
                            expected_generation,
                        }
                    },
                    None,
                )
            }
            "lifecycle/kill" => {
                let Some(input) = decode::<Kill>(bytes) else {
                    return error(StatusCode::BAD_REQUEST, "invalid_request");
                };
                let Ok(expected_generation) = GenerationId::parse_hex(&input.expected_generation)
                else {
                    return error(StatusCode::BAD_REQUEST, "invalid_request");
                };
                if input.acknowledgement
                    != format!("kill game:generation:{}", expected_generation.to_hex())
                {
                    return error(StatusCode::BAD_REQUEST, "confirmation_required");
                }
                (
                    GameCommand::ForceKill {
                        expected_generation,
                    },
                    input.confirmation_token,
                )
            }
            _ => unreachable!("closed actions"),
        };
        let admitted = match runtime.submit_game(&subject, command, &scoped_key) {
            Ok(a) => a,
            Err(e) => return operation_error(e),
        };
        let mut value = match admitted {
            Admission::Created(v) | Admission::Existing(v) => v,
        };
        if value.state == OperationState::AwaitingConfirmation {
            if let Some(token) = confirmation {
                let token = match ConfirmationToken::try_from(token.into_bytes()) {
                    Ok(t) => t,
                    Err(e) => return operation_error(e),
                };
                value = match runtime.confirm(&subject, &value.id, token) {
                    Ok(v) => v,
                    Err(e) => return operation_error(e),
                };
            } else {
                let receipt =
                    match runtime.issue_confirmation(&subject, &value.id, Duration::from_secs(120))
                    {
                        Ok(r) => r,
                        Err(e) => return operation_error(e),
                    };
                return envelope(
                    StatusCode::ACCEPTED,
                    json!({"data":{"operation_id":value.id.as_str(), "state":"awaiting_confirmation", "confirmation_token":receipt.token.expose_secret(), "expires_at_ms":receipt.expires_at_ms}}),
                );
            }
        }
        return envelope(StatusCode::ACCEPTED, json!({"data":summary(&value)}));
    }
    match action {
        "operations" => {
            if let Some(selection) = query.and_then(|query| query.strip_prefix("ids=")) {
                if selection.len() > 8192 {
                    return error(StatusCode::BAD_REQUEST, "invalid_request");
                }
                let ids = selection
                    .split(',')
                    .take(33)
                    .map(OperationId::parse)
                    .collect::<Result<Vec<_>, _>>();
                let ids = match ids {
                    Ok(ids) if !ids.is_empty() && ids.len() <= 32 => ids,
                    _ => return error(StatusCode::BAD_REQUEST, "invalid_request"),
                };
                return match operations.activity_get_many(&subject, &ids) {
                    Ok(items) => match items
                        .iter()
                        .map(|item| activity(host, item))
                        .collect::<Result<Vec<_>, _>>()
                    {
                        Ok(items) => ok(json!({"items":items,"selected":true})),
                        Err(error) => management_error(error),
                    },
                    Err(error) => operation_error(error),
                };
            }
            let mut kind = None;
            let mut actor_me = false;
            let mut paging = Vec::new();
            if let Some(query) = query {
                if query.is_empty() || query.len() > 700 {
                    return error(StatusCode::BAD_REQUEST, "invalid_request");
                }
                for field in query.split('&') {
                    if field.is_empty() {
                        return error(StatusCode::BAD_REQUEST, "invalid_request");
                    }
                    match field.split_once('=') {
                        Some(("kind", value))
                            if kind.is_none()
                                && vesta_control::operations::valid_activity_kind(value) =>
                        {
                            kind = Some(value)
                        }
                        Some(("actor", "me")) if !actor_me => actor_me = true,
                        Some(("kind" | "actor", _)) => {
                            return error(StatusCode::BAD_REQUEST, "invalid_request");
                        }
                        _ => paging.push(field),
                    }
                }
            }
            let paging = paging.join("&");
            let Some((after, limit)) = page(
                (!paging.is_empty()).then_some(paging.as_str()),
                "operations",
                host.management().server_id(),
                100,
            ) else {
                return error(StatusCode::BAD_REQUEST, "invalid_request");
            };
            let Ok(after) = i64::try_from(after) else {
                return error(StatusCode::BAD_REQUEST, "invalid_request");
            };
            match operations.activity_list(&subject, after, limit + 1, kind, actor_me) {
                Ok(mut items) => {
                    let more = items.len() > limit;
                    items.truncate(limit);
                    let next = if more {
                        items
                            .last()
                            .map(|v| cursor(host.management().server_id(), "operations", v.0.seq))
                    } else {
                        None
                    };
                    match items
                        .iter()
                        .map(|v| activity(host, v))
                        .collect::<Result<Vec<_>, _>>()
                    {
                        Ok(items) => ok(json!({"items":items,"next_cursor":next})),
                        Err(e) => management_error(e),
                    }
                }
                Err(e) => operation_error(e),
            }
        }
        "audit" => {
            let Some((after, limit)) = page(query, "audit", host.management().server_id(), 200)
            else {
                return error(StatusCode::BAD_REQUEST, "invalid_request");
            };
            let Ok(after) = i64::try_from(after) else {
                return error(StatusCode::BAD_REQUEST, "invalid_request");
            };
            match operations.audit_notices(&subject, after, limit) {
                Ok(mut items) => {
                    for item in &mut items {
                        if let Err(e) = redact_actor(host, &mut item.actor) {
                            return management_error(e);
                        }
                        if let Some(target) = &mut item.target {
                            match redact_text(host, target) {
                                Ok(v) => *target = v,
                                Err(e) => return management_error(e),
                            }
                        }
                        item.cursor = cursor(host.management().server_id(), "audit", &item.cursor);
                    }
                    let next = items.last().map(|v| v.cursor.clone());
                    ok(json!({"items":items,"next_cursor":next}))
                }
                Err(e) => operation_error(e),
            }
        }
        "console" => {
            let Some((after, limit)) = page(query, "console", host.management().server_id(), 200)
            else {
                return error(StatusCode::BAD_REQUEST, "invalid_request");
            };
            let after = if query
                .is_none_or(|q| !q.split('&').any(|field| field.starts_with("cursor=")))
            {
                match runtime.console_snapshot(0, 1) {
                    Ok(page) => page.latest_cursor.saturating_sub(limit as u64),
                    Err(_) => {
                        return error(StatusCode::SERVICE_UNAVAILABLE, "temporarily_unavailable");
                    }
                }
            } else {
                after
            };
            match runtime.console_snapshot(after, limit) {
                Ok(page) if !page.gap => {
                    let items = page.lines.iter().map(|v| Ok(json!({"cursor":cursor(host.management().server_id(), "console", v.cursor), "text":redact_console(host,&v.text)?, "truncated":v.truncated}))).collect::<Result<Vec<_>,ManagementError>>();
                    let items = match items {
                        Ok(items) => items,
                        Err(e) => return management_error(e),
                    };
                    ok(
                        json!({"items":items,"next_cursor":cursor(host.management().server_id(), "console", page.next_cursor),"latest_cursor":cursor(host.management().server_id(), "console", page.latest_cursor)}),
                    )
                }
                Ok(_) => error(StatusCode::CONFLICT, "cursor_gap"),
                Err(_) => error(StatusCode::BAD_REQUEST, "invalid_request"),
            }
        }
        "players" => {
            if query.is_some() {
                return error(StatusCode::BAD_REQUEST, "invalid_request");
            }
            let snapshot = match runtime.game_snapshot() {
                Ok(v) => v,
                Err(e) => return operation_error(e),
            };
            let Some(generation) = snapshot.generation else {
                return ok(json!({"known":false,"player_count":null,"items":null}));
            };
            match runtime.management_roster(&subject, generation, Duration::from_secs(1)) {
                Ok(v) => ok(
                    json!({"known":true,"generation":v.generation.to_hex(),"player_count":v.players.len(),"items":v.players.iter().filter(|p| p.len() <= 16 && p.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')).map(|p| json!({"name":p})).collect::<Vec<_>>()}),
                ),
                Err(_) => ok(json!({"known":false,"player_count":null,"items":null})),
            }
        }
        "pack" => {
            if query.is_some() {
                return error(StatusCode::BAD_REQUEST, "invalid_request");
            }
            match host.releases().current() {
                Ok(Some(v)) => ok(
                    json!({"hosted_revision":v.revision().to_string(),"minimum_revision":v.minimum_revision().to_string(),"status":"published"}),
                ),
                Ok(None) => ok(
                    json!({"hosted_revision":null,"minimum_revision":null,"status":"unpublished"}),
                ),
                Err(_) => error(StatusCode::SERVICE_UNAVAILABLE, "temporarily_unavailable"),
            }
        }
        _ => {
            if query.is_some() {
                return error(StatusCode::BAD_REQUEST, "invalid_request");
            }
            let Ok(id) =
                OperationId::parse(action.strip_prefix("operations/").expect("operation path"))
            else {
                return error(StatusCode::BAD_REQUEST, "invalid_request");
            };
            match operations.activity_get(&subject, &id) {
                Ok(Some(v)) => match activity(host, &v) {
                    Ok(v) => ok(v),
                    Err(e) => management_error(e),
                },
                Ok(None) => error(StatusCode::NOT_FOUND, "resource_not_found"),
                Err(e) => operation_error(e),
            }
        }
    }
}
