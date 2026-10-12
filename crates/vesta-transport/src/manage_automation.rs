//! Closed management adapter for workflows, the inbox and inspection backups.
use super::{Request, decode, envelope, error, management_error, ok};
use crate::Reply;
use http::{Method, StatusCode};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use vesta_control::{
    ControlError, IdentityErrorCode,
    automation::{
        Mutation, MutationError, MutationOutcome, Notification, Run, RunStep, Workflow,
        WorkflowDefinition,
    },
    backups::{BackupError, Job, JobKind, restic},
    config::{BackupMode, MutationPolicy},
    host::NetworkHost,
    identity::{Capability, PrincipalId, Subject},
};

const MAX_BODY: usize = 70 * 1024;
#[derive(Clone, Copy)]
enum Route<'a> {
    Workflows,
    Workflow(&'a str),
    Enabled(&'a str),
    Manual(&'a str),
    WorkflowRuns(&'a str),
    Run(&'a str),
    Steps(&'a str),
    Discard(&'a str),
    Notifications,
    Unread,
    Read,
    Acknowledge,
    Backups,
    BackupJob(&'a str),
    Snapshots,
    Backup,
    Verify,
    Restore,
}
fn resource<'a>(action: &'a str, prefix: &str) -> Option<&'a str> {
    let id = action.strip_prefix(prefix)?;
    (!id.is_empty() && id.len() <= 128 && !id.contains('/')).then_some(id)
}
fn route(path: &str) -> Option<Route<'_>> {
    let action = path.strip_prefix("/api/v3/manage/")?;
    Some(match action {
        "automation/workflows" => Route::Workflows,
        "notifications" => Route::Notifications,
        "notifications/unread" => Route::Unread,
        "notifications/read" => Route::Read,
        "notifications/acknowledge" => Route::Acknowledge,
        "backups" => Route::Backups,
        "backups/snapshots" => Route::Snapshots,
        "backups/run" => Route::Backup,
        "backups/verify" => Route::Verify,
        "backups/restore" => Route::Restore,
        _ => {
            if let Some(id) = action
                .strip_suffix("/enabled")
                .and_then(|s| resource(s, "automation/workflows/"))
            {
                Route::Enabled(id)
            } else if let Some(id) = action
                .strip_suffix("/run")
                .and_then(|s| resource(s, "automation/workflows/"))
            {
                Route::Manual(id)
            } else if let Some(id) = action
                .strip_suffix("/runs")
                .and_then(|s| resource(s, "automation/workflows/"))
            {
                Route::WorkflowRuns(id)
            } else if let Some(id) = action
                .strip_suffix("/steps")
                .and_then(|s| resource(s, "automation/runs/"))
            {
                Route::Steps(id)
            } else if let Some(id) = action
                .strip_suffix("/discard")
                .and_then(|s| resource(s, "automation/runs/"))
            {
                Route::Discard(id)
            } else if let Some(id) = resource(action, "automation/workflows/") {
                Route::Workflow(id)
            } else if let Some(id) = resource(action, "automation/runs/") {
                Route::Run(id)
            } else if let Some(id) = resource(action, "backups/jobs/") {
                Route::BackupJob(id)
            } else {
                return None;
            }
        }
    })
}
pub(super) fn implemented(path: &str) -> bool {
    route(path).is_some()
}
fn invalid() -> Reply {
    error(StatusCode::BAD_REQUEST, "invalid_request")
}
fn control_failure(value: ControlError, precondition: bool) -> Reply {
    match value {
        ControlError::Rejected {
            code: IdentityErrorCode::Forbidden,
            ..
        } => error(StatusCode::FORBIDDEN, "management_forbidden"),
        ControlError::Rejected {
            code: IdentityErrorCode::InvalidRequest,
            ..
        } => invalid(),
        ControlError::Rejected {
            code: IdentityErrorCode::NotFound,
            ..
        } => error(StatusCode::NOT_FOUND, "resource_not_found"),
        ControlError::Rejected {
            code: IdentityErrorCode::Conflict,
            ..
        } if precondition => error(StatusCode::PRECONDITION_FAILED, "precondition_failed"),
        ControlError::Rejected {
            code: IdentityErrorCode::Conflict,
            ..
        } => error(StatusCode::CONFLICT, "automation_conflict"),
        _ => error(StatusCode::SERVICE_UNAVAILABLE, "temporarily_unavailable"),
    }
}
fn mutation_failure(value: MutationError, precondition: bool) -> Reply {
    match value {
        MutationError::Control(e) => control_failure(e, precondition),
        MutationError::IdempotencyConflict => error(StatusCode::CONFLICT, "idempotency_conflict"),
        _ => error(StatusCode::SERVICE_UNAVAILABLE, "temporarily_unavailable"),
    }
}
fn backup_failure(value: BackupError) -> Reply {
    let status = match value {
        BackupError::Disabled | BackupError::UnknownProfile | BackupError::UnknownJob => {
            StatusCode::NOT_FOUND
        }
        BackupError::Forbidden => StatusCode::FORBIDDEN,
        BackupError::Invalid => StatusCode::BAD_REQUEST,
        BackupError::Busy | BackupError::IdempotencyConflict => StatusCode::CONFLICT,
        BackupError::Unavailable | BackupError::State => StatusCode::SERVICE_UNAVAILABLE,
    };
    error(status, value.code())
}
// Restic metadata is untrusted output, not a free-form response channel.
// Accept its canonical RFC3339/RFC3339Nano date-time form without a new crate.
fn snapshot_time(value: &str) -> bool {
    if !(20..=35).contains(&value.len()) || !value.is_ascii() {
        return false;
    }
    let b = value.as_bytes();
    if b[4] != b'-' || b[7] != b'-' || b[10] != b'T' || b[13] != b':' || b[16] != b':' {
        return false;
    }
    for (start, end) in [(0, 4), (5, 7), (8, 10), (11, 13), (14, 16), (17, 19)] {
        if !b[start..end].iter().all(u8::is_ascii_digit) {
            return false;
        }
    }
    let number = |start: usize, end: usize| {
        b[start..end]
            .iter()
            .fold(0_u16, |n, v| n * 10 + u16::from(v - b'0'))
    };
    let year = number(0, 4);
    let month = number(5, 7);
    let days = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400)) => {
            29
        }
        2 => 28,
        _ => return false,
    };
    if !(1..=days).contains(&number(8, 10))
        || number(11, 13) > 23
        || number(14, 16) > 59
        || number(17, 19) > 59
    {
        return false;
    }
    let mut zone = 19;
    if b[zone] == b'.' {
        zone += 1;
        let start = zone;
        while zone < b.len() && b[zone].is_ascii_digit() {
            zone += 1;
        }
        if !(1..=9).contains(&(zone - start)) {
            return false;
        }
    }
    let suffix = &b[zone..];
    suffix == b"Z"
        || (suffix.len() == 6
            && matches!(suffix[0], b'+' | b'-')
            && suffix[3] == b':'
            && [suffix[1], suffix[2], suffix[4], suffix[5]]
                .iter()
                .all(u8::is_ascii_digit)
            && (suffix[1] - b'0') * 10 + suffix[2] - b'0' <= 23
            && (suffix[4] - b'0') * 10 + suffix[5] - b'0' <= 59)
}
fn backup_read_failure(value: BackupError, snapshot_listing: bool) -> Reply {
    if snapshot_listing && matches!(value, BackupError::Busy) {
        error(StatusCode::SERVICE_UNAVAILABLE, "temporarily_unavailable")
    } else {
        backup_failure(value)
    }
}
fn capability(route: Route<'_>, method: &Method) -> Capability {
    match route {
        Route::Notifications | Route::Unread | Route::Read | Route::Acknowledge => {
            Capability::NotificationsView
        }
        Route::Backups | Route::BackupJob(_) | Route::Snapshots => Capability::BackupsView,
        Route::Backup => Capability::BackupsCreate,
        Route::Verify => Capability::BackupsVerify,
        Route::Restore => Capability::BackupsRestore,
        _ if method == Method::GET => Capability::AutomationView,
        _ => Capability::AutomationManage,
    }
}
/// Bind keys to all identity/request dimensions. Backup keys retain an explicit
/// principal prefix and fit the backup service's 200-byte limit even for 128-byte IDs.
fn scoped_key(host: &NetworkHost, subject: &Subject, request: &Request<'_>) -> Option<String> {
    let key = request.idempotency?;
    if !(16..=128).contains(&key.len()) || !key.bytes().all(|b| b.is_ascii_graphic()) {
        return None;
    }
    let mut hash = Sha256::new();
    for part in [
        host.database().server_id(),
        subject.principal.as_str(),
        subject.device.as_str(),
        request.method.as_str(),
        request.path,
        key,
    ] {
        hash.update((part.len() as u64).to_be_bytes());
        hash.update(part.as_bytes());
    }
    Some(format!("{:x}", hash.finalize()))
}
fn query_parts(query: Option<&str>) -> Option<Vec<(&str, &str)>> {
    let Some(query) = query else {
        return Some(Vec::new());
    };
    if query.is_empty() || query.len() > 600 {
        return None;
    }
    let mut parts = Vec::new();
    for field in query.split('&') {
        let (key, value) = field.split_once('=')?;
        if key.is_empty()
            || value.is_empty()
            || parts.iter().any(|(prior, _)| *prior == key)
            || parts.len() >= 3
        {
            return None;
        }
        parts.push((key, value));
    }
    Some(parts)
}
fn number(value: &str) -> Option<i64> {
    if value.is_empty()
        || value.len() > 19
        || !value.bytes().all(|b| b.is_ascii_digit())
        || (value.len() > 1 && value.starts_with('0'))
    {
        return None;
    }
    value.parse().ok()
}
fn id_page(query: Option<&str>) -> Option<(Option<&str>, usize)> {
    let (mut after, mut limit) = (None, 50);
    for (key, value) in query_parts(query)? {
        match key {
            "after" if PrincipalId::parse(value).is_ok() => after = Some(value),
            "limit" => limit = usize::try_from(number(value)?).ok()?,
            _ => return None,
        }
    }
    (1..=100).contains(&limit).then_some((after, limit))
}
fn numeric_page(
    query: Option<&str>,
    cursor_name: &str,
    maximum: usize,
) -> Option<(Option<i64>, usize)> {
    let (mut cursor, mut limit) = (None, 50);
    for (key, value) in query_parts(query)? {
        if key == cursor_name {
            cursor = Some(number(value)?);
        } else if key == "limit" {
            limit = usize::try_from(number(value)?).ok()?;
        } else {
            return None;
        }
    }
    (1..=maximum).contains(&limit).then_some((cursor, limit))
}
fn backup_cursor(timestamp: i64, id: &str) -> String {
    let encoded = id
        .as_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    format!("b1.{timestamp:016x}.{encoded}")
}
fn parse_backup_cursor(cursor: &str) -> Option<(i64, String)> {
    let (timestamp, id) = cursor.strip_prefix("b1.")?.split_once('.')?;
    let hex = |b: u8| match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        _ => None,
    };
    if timestamp.len() != 16
        || !timestamp.bytes().all(|b| hex(b).is_some())
        || id.is_empty()
        || id.len() > 256
        || !id.len().is_multiple_of(2)
    {
        return None;
    }
    let timestamp = i64::try_from(u64::from_str_radix(timestamp, 16).ok()?).ok()?;
    let mut decoded = Vec::with_capacity(id.len() / 2);
    for pair in id.as_bytes().chunks_exact(2) {
        decoded.push(hex(pair[0])? * 16 + hex(pair[1])?);
    }
    let id = String::from_utf8(decoded).ok()?;
    PrincipalId::parse(&id).ok()?;
    Some((timestamp, id))
}
fn backup_page(query: Option<&str>) -> Option<(Option<(i64, String)>, usize)> {
    let (mut before, mut limit) = (None, 50);
    for (key, value) in query_parts(query)? {
        match key {
            "before" => before = Some(parse_backup_cursor(value)?),
            "limit" => limit = usize::try_from(number(value)?).ok()?,
            _ => return None,
        }
    }
    (1..=200).contains(&limit).then_some((before, limit))
}
fn restore_path(value: &str) -> bool {
    restic::literal_path(value) && vesta_control::files::RelativePath::parse(value).is_ok()
}
fn profile(value: &str) -> bool {
    (1..=32).contains(&value.len())
        && !value.starts_with('-')
        && value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}
fn workflow(value: &Workflow, detailed: bool) -> Value {
    let mut result = json!({"id":value.id,"name":value.name,"enabled":value.enabled,"execution_principal_id":value.execution_principal.as_str(),"created_at_ms":value.created_at,"updated_at_ms":value.updated_at,"revision":value.revision});
    if detailed {
        result["definition"] = json!(value.definition);
    }
    result
}
fn run(value: &Run, detailed: bool) -> Value {
    let mut result = json!({"id":value.id,"workflow_id":value.workflow_id,"workflow_revision":value.revision,"execution_principal_id":value.execution_principal.as_str(),"trigger":value.trigger,"state":value.state,"current_step":value.current_step,"attempt":value.attempt,"next_wake_at_ms":value.next_wake_at,"started_at_ms":value.started_at,"finished_at_ms":value.finished_at,"outcome_code":value.outcome_code});
    if detailed {
        result["definition"] = json!(value.definition);
    }
    result
}
fn step(value: &RunStep) -> Value {
    json!({"run_id":value.run_id,"index":value.index,"attempt":value.attempt,"cursor":value.index*6+usize::from(value.attempt),"state":value.state,"operation_reference":value.operation_reference,"output":value.output,"error_code":value.error_code,"started_at_ms":value.started_at,"finished_at_ms":value.finished_at})
}
fn notification(value: &Notification) -> Value {
    json!({"id":value.id,"cursor":value.cursor,"created_at_ms":value.created_at,"severity":value.severity,"title":value.title,"body":value.body,"source":value.source,"read":value.read,"acknowledged":value.acknowledged})
}
fn safe_backup_code(value: &str) -> &'static str {
    match value {
        "backup_engine_unavailable" => "backup_engine_unavailable",
        "backup_engine_failed" => "backup_engine_failed",
        "backup_source_incomplete" => "backup_source_incomplete",
        "backup_repository_locked" => "backup_repository_locked",
        "backup_engine_timed_out" => "backup_engine_timed_out",
        "backup_engine_protocol" => "backup_engine_protocol",
        "backup_invalid_request" => "backup_invalid_request",
        "backup_source_changed" => "backup_source_changed",
        "backups_not_configured" => "backups_not_configured",
        "backup_profile_unknown" => "backup_profile_unknown",
        "backup_job_unknown" => "backup_job_unknown",
        "backup_busy" => "backup_busy",
        "backup_state_unavailable" => "backup_state_unavailable",
        "backup_idempotency_conflict" => "backup_idempotency_conflict",
        "backup_forbidden" => "backup_forbidden",
        "backup_interrupted" => "backup_interrupted",
        "backup_requires_stopped_server" => "backup_requires_stopped_server",
        "backup_online_requires_supervisor" => "backup_online_requires_supervisor",
        "backup_online_requires_running_server" => "backup_online_requires_running_server",
        "backup_save_suspend_refused" => "backup_save_suspend_refused",
        "backup_save_suspend_failed" => "backup_save_suspend_failed",
        "backup_restore_target_unavailable" => "backup_restore_target_unavailable",
        "backup_ignore_file_invalid" => "backup_ignore_file_invalid",
        _ => "backup_unavailable",
    }
}
fn backup_job(host: &NetworkHost, token: &str, value: &Job) -> Value {
    let mut result = json!({"id":value.id,"profile":value.profile,"kind":value.kind,"state":value.state_wire(),"requested_by":value.requested_by,"snapshot_id":value.snapshot_id,"error_code":value.error_code.as_deref().map(safe_backup_code),"created_at_ms":value.created_at,"started_at_ms":value.started_at,"finished_at_ms":value.finished_at});
    let Some(raw) = value.result.as_ref().and_then(Value::as_object) else {
        result["result"] = Value::Null;
        return result;
    };
    let mut facts = serde_json::Map::new();
    for key in [
        "files_new",
        "files_changed",
        "files_unmodified",
        "data_added",
        "total_bytes_processed",
        "source_changes",
    ] {
        if let Some(v) = raw.get(key).and_then(Value::as_u64) {
            facts.insert(key.into(), json!(v));
        }
    }
    for key in ["world_changed", "verified", "truncated"] {
        if let Some(v) = raw.get(key).and_then(Value::as_bool) {
            facts.insert(key.into(), json!(v));
        }
    }
    if let Some(id) = raw
        .get("snapshot_id")
        .and_then(Value::as_str)
        .filter(|id| restic::valid_snapshot_id(id))
    {
        facts.insert("snapshot_id".into(), json!(id));
    }
    if let Some(consistency) = raw
        .get("consistency")
        .and_then(Value::as_str)
        .filter(|v| matches!(*v, "world_save_consistent" | "stopped"))
    {
        facts.insert("consistency".into(), json!(consistency));
    }
    if let Some(code) = raw.get("retention_error") {
        facts.insert(
            "retention_error".into(),
            code.as_str()
                .map_or(Value::Null, |code| json!(safe_backup_code(code))),
        );
    }
    if value.kind == "restore"
        && let Some(root) = host
            .backups()
            .and_then(|b| b.settings().restore_root.as_ref())
    {
        let expected = root.join(&value.id).to_string_lossy().into_owned();
        if raw.get("restored_to").and_then(Value::as_str) == Some(expected.as_str()) {
            facts.insert("restored_to".into(), json!(expected));
        }
    }
    if let Some(paths) = raw
        .get("changed_paths")
        .and_then(Value::as_array)
        .filter(|v| v.len() <= 50)
    {
        let authorized = paths.iter().all(|v| {
            v.as_str().is_some_and(|path| {
                vesta_control::files::RelativePath::parse(path).is_ok()
                    && vesta_control::identity::Resource::parse(path).is_ok_and(|resource| {
                        host.management()
                            .authorize(token, Capability::FilesRead, Some(&resource))
                            .is_ok()
                    })
            })
        });
        if authorized {
            facts.insert("changed_paths".into(), json!(paths));
        }
    }
    result["result"] = Value::Object(facts);
    result
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Create {
    name: String,
    execution_principal_id: String,
    definition: WorkflowDefinition,
    enabled: bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Update {
    expected_revision: i64,
    name: String,
    execution_principal_id: String,
    definition: WorkflowDefinition,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Revision {
    expected_revision: i64,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Enabled {
    expected_revision: i64,
    enabled: bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Empty {}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Ids {
    ids: Vec<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Profile {
    profile: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Restore {
    profile: String,
    snapshot: String,
    paths: Vec<String>,
}

pub(super) fn dispatch(host: &NetworkHost, request: Request<'_>) -> Reply {
    let Some(route) = route(request.path) else {
        return invalid();
    };
    let Some(token) = request.authorization else {
        return error(StatusCode::UNAUTHORIZED, "management_session_invalid");
    };
    let subject = match host
        .management()
        .authorize(token, capability(route, request.method), None)
    {
        Ok(s) => s,
        Err(e) => return management_error(e),
    };
    if request.bytes.len() > MAX_BODY {
        return error(StatusCode::PAYLOAD_TOO_LARGE, "request_too_large");
    }
    if request.method == Method::GET {
        if !request.bytes.is_empty() {
            return invalid();
        }
        return get(host, token, &subject, route, request.query);
    }
    if request.query.is_some() {
        return invalid();
    }
    let Some(key) = scoped_key(host, &subject, &request) else {
        return invalid();
    };
    if matches!(route, Route::Backup | Route::Verify | Route::Restore) {
        if request.method != Method::POST {
            return invalid();
        }
        return submit_backup(host, &subject, route, request.bytes, &key);
    }
    let command = match (request.method.as_str(), route) {
        ("POST", Route::Workflows) => decode::<Create>(request.bytes).map(|v| Mutation::Create {
            name: v.name,
            execution_principal: v.execution_principal_id,
            definition: v.definition,
            enabled: v.enabled,
        }),
        ("PUT", Route::Workflow(id)) => decode::<Update>(request.bytes).map(|v| Mutation::Update {
            id: id.into(),
            expected_revision: v.expected_revision,
            name: v.name,
            execution_principal: v.execution_principal_id,
            definition: v.definition,
        }),
        ("DELETE", Route::Workflow(id)) => {
            decode::<Revision>(request.bytes).map(|v| Mutation::Delete {
                id: id.into(),
                expected_revision: v.expected_revision,
            })
        }
        ("POST", Route::Enabled(id)) => {
            decode::<Enabled>(request.bytes).map(|v| Mutation::SetEnabled {
                id: id.into(),
                expected_revision: v.expected_revision,
                enabled: v.enabled,
            })
        }
        ("POST", Route::Manual(id)) => {
            decode::<Empty>(request.bytes).map(|_| Mutation::RunManual { id: id.into() })
        }
        ("POST", Route::Discard(id)) => {
            decode::<Empty>(request.bytes).map(|_| Mutation::Discard { id: id.into() })
        }
        ("POST", Route::Read) => {
            decode::<Ids>(request.bytes).map(|v| Mutation::MarkRead { ids: v.ids })
        }
        ("POST", Route::Acknowledge) => {
            decode::<Ids>(request.bytes).map(|v| Mutation::Acknowledge { ids: v.ids })
        }
        _ => None,
    };
    let Some(command) = command else {
        return invalid();
    };
    let precondition = matches!(
        command,
        Mutation::Update { .. } | Mutation::Delete { .. } | Mutation::SetEnabled { .. }
    );
    match host.automation().apply_idempotent(&subject, &key, command) {
        Ok(outcome) => {
            let status = if matches!(
                outcome,
                MutationOutcome::Run {
                    run_id: Some(_),
                    ..
                }
            ) {
                StatusCode::ACCEPTED
            } else if matches!(route, Route::Workflows) {
                StatusCode::CREATED
            } else {
                StatusCode::OK
            };
            envelope(status, json!({"data":outcome}))
        }
        Err(e) => mutation_failure(e, precondition),
    }
}
fn get(
    host: &NetworkHost,
    token: &str,
    subject: &Subject,
    route: Route<'_>,
    query: Option<&str>,
) -> Reply {
    let automation = host.automation();
    let result: std::result::Result<Value, ControlError> = match route {
        Route::Workflows => {
            let Some((after, limit)) = id_page(query) else {
                return invalid();
            };
            automation.list_workflows(subject,after,limit).map(|v|json!({"workflows":v.iter().map(|v|workflow(v,false)).collect::<Vec<_>>(),"next_after":if v.len()==limit {v.last().map(|v|v.id.as_str())} else {None}}))
        }
        Route::Workflow(id) if query.is_none() => automation
            .get_workflow(subject, id)
            .map(|v| json!({"workflow":workflow(&v,true)})),
        Route::WorkflowRuns(id) => {
            let Some((after, limit)) = id_page(query) else {
                return invalid();
            };
            automation.list_runs(subject,Some(id),after,limit).map(|v|json!({"runs":v.iter().map(|v|run(v,false)).collect::<Vec<_>>(),"next_after":if v.len()==limit {v.last().map(|v|v.id.as_str())} else {None}}))
        }
        Route::Run(id) if query.is_none() => automation
            .get_run(subject, id)
            .map(|v| json!({"run":run(&v,true)})),
        Route::Steps(id) => {
            let Some((after, limit)) = numeric_page(query, "after", 100) else {
                return invalid();
            };
            let after = match after.map(u16::try_from).transpose() {
                Ok(after) => after,
                Err(_) => return invalid(),
            };
            automation.list_steps(subject,id,after,limit).map(|v|json!({"steps":v.iter().map(step).collect::<Vec<_>>(),"next_after":if v.len()==limit {v.last().map(|v|v.index*6+usize::from(v.attempt))} else {None}}))
        }
        Route::Notifications => {
            let Some((cursor, limit)) = numeric_page(query, "cursor", 100) else {
                return invalid();
            };
            automation.list_since(subject,cursor.unwrap_or(0),limit).map(|v|json!({"notifications":v.notifications.iter().map(notification).collect::<Vec<_>>(),"next_cursor":v.next_cursor,"oldest_cursor":v.oldest_cursor}))
        }
        Route::Unread if query.is_none() => automation
            .unread_count(subject)
            .map(|count| json!({"unread_count":count})),
        Route::Backups | Route::BackupJob(_) | Route::Snapshots => {
            return get_backups(host, token, route, query);
        }
        _ => return invalid(),
    };
    match result {
        Ok(v) => ok(v),
        Err(e) => control_failure(e, false),
    }
}
fn get_backups(host: &NetworkHost, token: &str, route: Route<'_>, query: Option<&str>) -> Reply {
    enum Query<'a> {
        Jobs(Option<(i64, String)>, usize),
        Job(&'a str),
        Snapshots(&'a str),
    }
    let query = match route {
        Route::Backups => {
            let Some((before, limit)) = backup_page(query) else {
                return invalid();
            };
            Query::Jobs(before, limit)
        }
        Route::BackupJob(id) if query.is_none() && PrincipalId::parse(id).is_ok() => Query::Job(id),
        Route::Snapshots => {
            let Some(parts) = query_parts(query) else {
                return invalid();
            };
            let [("profile", value)] = parts.as_slice() else {
                return invalid();
            };
            if !profile(value) {
                return invalid();
            }
            Query::Snapshots(value)
        }
        _ => return invalid(),
    };
    let Some(backups) = host.backups() else {
        return backup_failure(BackupError::Disabled);
    };
    let snapshot_listing = matches!(&query, Query::Snapshots(_));
    let result=match query {
        Query::Jobs(before,limit)=>backups.jobs(before.as_ref().map(|(timestamp,id)|(*timestamp,id.as_str())),limit).map(|jobs| {
            let profiles=backups.settings().profiles.iter().map(|p|json!({"name":p.name,"mode":match p.mode {BackupMode::Online=>"online",BackupMode::Stopped=>"stopped"},"keep_last":p.keep_last,"keep_daily":p.keep_daily,"keep_weekly":p.keep_weekly,"keep_monthly":p.keep_monthly,"on_mutation":match p.on_mutation {MutationPolicy::Warn=>"warn",MutationPolicy::FailWorld=>"fail-world",MutationPolicy::FailAny=>"fail-any"},"warn_seconds":p.warn_seconds})).collect::<Vec<_>>();
            json!({"profiles":profiles,"jobs":jobs.iter().map(|v|backup_job(host,token,v)).collect::<Vec<_>>(),"next_before":if jobs.len()==limit {jobs.last().map(|v|backup_cursor(v.created_at,&v.id))} else {None},"restore_mode":"inspection_only"})
        }),
        Query::Job(id)=>match backups.job(id) {Ok(Some(job))=>Ok(json!({"job":backup_job(host,token,&job),"restore_mode":"inspection_only"})),Ok(None)=>Err(BackupError::UnknownJob),Err(e)=>Err(e)},
        Query::Snapshots(profile)=>backups.snapshots(profile).and_then(|snapshots| {
            if snapshots.iter().any(|v|!restic::valid_snapshot_id(&v.id)||!snapshot_time(&v.time)) {return Err(BackupError::Unavailable);}
            Ok(json!({"profile":profile,"snapshots":snapshots.iter().map(|v|json!({"id":v.id,"time":v.time})).collect::<Vec<_>>() }))
        }),
    };
    match result {
        Ok(v) => ok(v),
        Err(e) => backup_read_failure(e, snapshot_listing),
    }
}
fn submit_backup(
    host: &NetworkHost,
    subject: &Subject,
    route: Route<'_>,
    bytes: &[u8],
    key: &str,
) -> Reply {
    let (profile_name, kind) = match route {
        Route::Backup | Route::Verify => {
            let Some(value) = decode::<Profile>(bytes).filter(|v| profile(&v.profile)) else {
                return invalid();
            };
            (
                value.profile,
                if matches!(route, Route::Backup) {
                    JobKind::Backup
                } else {
                    JobKind::Verify
                },
            )
        }
        Route::Restore => {
            let Some(value) = decode::<Restore>(bytes) else {
                return invalid();
            };
            if !profile(&value.profile)
                || !restic::valid_snapshot_id(&value.snapshot)
                || value.paths.len() > 64
                || value.paths.iter().any(|p| !restore_path(p))
            {
                return invalid();
            }
            (
                value.profile,
                JobKind::Restore {
                    snapshot: value.snapshot,
                    paths: value.paths,
                },
            )
        }
        _ => return invalid(),
    };
    let Some(backups) = host.backups() else {
        return backup_failure(BackupError::Disabled);
    };
    let key = format!("{}:{key}", subject.principal.as_str());
    let kind_name = match &kind {
        JobKind::Backup => "backup",
        JobKind::Verify => "verify",
        JobKind::Restore { .. } => "restore",
    };
    match backups.submit(subject, &profile_name, kind, Some(&key)) {
        Ok(id) => envelope(
            StatusCode::ACCEPTED,
            json!({"data":{"job_id":id,"kind":kind_name,"restore_mode":"inspection_only"}}),
        ),
        Err(e) => backup_failure(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bounded_query_contract_rejects_duplicates_and_noncanonical_numbers() {
        for query in [
            "",
            "limit=0",
            "limit=101",
            "limit=01",
            "limit=-1",
            "limit=1&limit=2",
            "unknown=1",
            "after=x%2Fy",
            "limit=18446744073709551616",
        ] {
            assert!(id_page(Some(query)).is_none(), "{query}");
        }
        assert_eq!(id_page(None), Some((None, 50)));
        assert_eq!(
            id_page(Some("after=workflow-1&limit=100")),
            Some((Some("workflow-1"), 100))
        );
        assert_eq!(
            numeric_page(Some("cursor=0&limit=100"), "cursor", 100),
            Some((Some(0), 100))
        );
        assert!(numeric_page(Some("cursor=9223372036854775808"), "cursor", 100).is_none());
    }
    #[test]
    fn backup_composite_cursors_round_trip_ties_and_reject_ambiguous_input() {
        for (time, id) in [
            (0, "job-zero"),
            (1_791_115_200_000, "job-a"),
            (1_791_115_200_000, "job-b"),
            (i64::MAX, "job-last"),
        ] {
            let cursor = backup_cursor(time, id);
            assert_eq!(parse_backup_cursor(&cursor), Some((time, id.into())));
            assert_eq!(
                backup_page(Some(&format!("before={cursor}&limit=1"))),
                Some((Some((time, id.into())), 1))
            );
        }
        assert_ne!(backup_cursor(100, "job-a"), backup_cursor(100, "job-b"));
        for cursor in [
            "100",
            "b1.0000000000000000.",
            "b1.8000000000000000.61",
            "b1.0000000000000000.6",
            "b1.0000000000000000.2f",
            "b1.0000000000000000.FF",
            "b2.0000000000000000.61",
        ] {
            assert!(parse_backup_cursor(cursor).is_none(), "{cursor}");
        }
        assert!(backup_page(Some("limit=201")).is_none());
        assert!(backup_page(Some("limit=1&limit=2")).is_none());
        assert_eq!(backup_page(None), Some((None, 50)));
    }
    #[test]
    fn restore_targets_are_literal_confined_paths() {
        assert!(restore_path("world/region/r.0.0.mca"));
        for path in [
            "world/*",
            "world/file?",
            "world/[file]",
            "world/{file}",
            "../world",
            "/world",
        ] {
            assert!(!restore_path(path), "{path}");
        }
    }
    #[test]
    fn snapshot_admission_busy_is_unavailable_not_job_conflict() {
        assert_eq!(
            backup_read_failure(BackupError::Busy, true).status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
        assert_eq!(
            backup_read_failure(BackupError::Busy, false).status(),
            StatusCode::CONFLICT
        );
        assert_eq!(
            backup_failure(BackupError::Forbidden).status(),
            StatusCode::FORBIDDEN
        );
    }
    #[test]
    fn snapshot_dates_are_structured_and_cannot_echo_native_output() {
        for time in [
            "2026-10-12T01:02:03Z",
            "2000-02-29T23:59:59.123456789+14:00",
            "1900-02-28T00:00:00-12:30",
        ] {
            assert!(snapshot_time(time), "{time}");
        }
        for time in [
            "C:/private/password.txt",
            "restic failed unexpectedly",
            "1900-02-29T00:00:00Z",
            "2026-04-31T00:00:00Z",
            "2026-10-12T24:00:00Z",
            "2026-10-12T00:00:00+24:00",
            "2026-10-12T00:00:00.Z",
            "2026-10-12T00:00:00Z/secret",
            "2026-10-12T00:00:00",
        ] {
            assert!(!snapshot_time(time), "{time}");
        }
    }
    #[test]
    fn backup_error_projection_uses_only_fixed_codes() {
        assert_eq!(safe_backup_code("backup_interrupted"), "backup_interrupted");
        assert_eq!(
            safe_backup_code("backup_online_requires_supervisor"),
            "backup_online_requires_supervisor"
        );
        for text in [
            r"C:\private\password.txt",
            "restic failed: secret-token",
            "backup_arbitrary_diagnostic",
            "",
        ] {
            assert_eq!(safe_backup_code(text), "backup_unavailable");
        }
    }
    #[test]
    fn route_namespace_is_closed_and_restore_body_is_strict() {
        assert!(implemented(
            "/api/v3/manage/automation/workflows/workflow-1/enabled"
        ));
        assert!(!implemented(
            "/api/v3/manage/automation/workflows/workflow-1/extra"
        ));
        assert!(!implemented("/api/v3/manage/notifications/produce"));
        assert!(!implemented("/api/v3/manage/backups/execute"));
        assert!(
            decode::<Restore>(
                br#"{"profile":"daily","snapshot":"12345678","paths":[],"requested_by":"owner"}"#
            )
            .is_none()
        );
    }
}
