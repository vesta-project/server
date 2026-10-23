//! Closed management adapter for workflows, the inbox and backup jobs.
use super::{Request, decode, envelope, error, management_error, ok};
use crate::Reply;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
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
    backups::{BackupError, Job, JobKind, LimitsDocument, PolicyDocument, ProfileDocument, engine},
    config::{BackupMode, MutationPolicy},
    host::NetworkHost,
    identity::{Capability, PrincipalId, Subject},
    operations::ConfirmationToken,
};

const MAX_BODY: usize = 70 * 1024;
#[derive(Clone, Copy)]
enum Route<'a> {
    Workflows,
    Workflow(&'a str),
    Enabled(&'a str),
    Manual(&'a str),
    WorkflowRuns(&'a str),
    Runs,
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
    Snapshot(&'a str),
    SafetySnapshots,
    Storage,
    Policy,
    PolicyReset,
    RecoveryExport,
    RestoreInPlace,
    ConfirmRestore(&'a str),
    ResolveRecovery(&'a str),
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
        "automation/runs" => Route::Runs,
        "notifications" => Route::Notifications,
        "notifications/unread" => Route::Unread,
        "notifications/read" => Route::Read,
        "notifications/acknowledge" => Route::Acknowledge,
        "backups" => Route::Backups,
        "backups/snapshots" => Route::Snapshots,
        "backups/safety-snapshots" => Route::SafetySnapshots,
        "backups/storage" => Route::Storage,
        "backups/policy" => Route::Policy,
        "backups/policy/reset" => Route::PolicyReset,
        "recovery/export" => Route::RecoveryExport,
        "backups/restore-in-place" => Route::RestoreInPlace,
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
            } else if let Some(id) = action
                .strip_suffix("/confirm")
                .and_then(|s| resource(s, "backups/jobs/"))
            {
                Route::ConfirmRestore(id)
            } else if let Some(id) = action
                .strip_suffix("/resolve-recovery")
                .and_then(|s| resource(s, "backups/jobs/"))
            {
                Route::ResolveRecovery(id)
            } else if let Some(id) = resource(action, "backups/snapshots/") {
                Route::Snapshot(id)
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
        BackupError::Busy
        | BackupError::IdempotencyConflict
        | BackupError::ConfirmationInvalid
        | BackupError::PolicyConflict => StatusCode::CONFLICT,
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
        Route::Policy if *method == Method::GET => Capability::BackupsView,
        Route::Policy | Route::PolicyReset => Capability::BackupsConfigure,
        Route::RecoveryExport => Capability::RecoveryExport,
        Route::Notifications | Route::Unread | Route::Read | Route::Acknowledge => {
            Capability::NotificationsView
        }
        Route::Backups
        | Route::BackupJob(_)
        | Route::Snapshots
        | Route::Snapshot(_)
        | Route::SafetySnapshots
        | Route::Storage => Capability::BackupsView,
        Route::Backup => Capability::BackupsCreate,
        Route::Verify => Capability::BackupsVerify,
        Route::Restore
        | Route::RestoreInPlace
        | Route::ConfirmRestore(_)
        | Route::ResolveRecovery(_) => Capability::BackupsRestore,
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
    engine::literal_path(value) && vesta_control::files::RelativePath::parse(value).is_ok()
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
    result["trigger"] = json!(value.definition.trigger);
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
        "backup_confirmation_invalid" => "backup_confirmation_invalid",
        "backup_confirmation_expired" => "backup_confirmation_expired",
        "backup_snapshot_not_restorable" => "backup_snapshot_not_restorable",
        "backup_snapshot_unknown" => "backup_snapshot_unknown",
        "backup_restore_path_not_in_snapshot" => "backup_restore_path_not_in_snapshot",
        "backup_restore_target_unsafe" => "backup_restore_target_unsafe",
        "backup_restore_restart_pending" => "backup_restore_restart_pending",
        "backup_safety_snapshot_failed" => "backup_safety_snapshot_failed",
        "backup_restore_interrupted" => "backup_restore_interrupted",
        "backup_authority_revoked" => "backup_authority_revoked",
        "backup_consistency_lost" => "backup_consistency_lost",
        "backup_disk_space_low" => "backup_disk_space_low",
        "backup_repository_corrupt" => "backup_repository_corrupt",
        "backup_repository_unavailable" => "backup_repository_unavailable",
        "backup_snapshot_limit_reached" => "backup_snapshot_limit_reached",
        "backup_storage_limit_reached" => "backup_storage_limit_reached",
        "backup_storage_unavailable" => "backup_storage_unavailable",
        "backup_tag_failed" => "backup_tag_failed",
        "backup_too_large" => "backup_too_large",
        _ => "backup_unavailable",
    }
}
fn backup_job_metadata(value: &Job) -> Value {
    json!({"id":value.id,"profile":value.profile,"kind":value.kind,"state":value.state_wire(),"requested_by":value.requested_by,"snapshot_id":value.snapshot_id,"error_code":value.error_code.as_deref().map(safe_backup_code),"created_at_ms":value.created_at,"started_at_ms":value.started_at,"finished_at_ms":value.finished_at})
}
fn in_place_facts(raw: &serde_json::Map<String, Value>) -> serde_json::Map<String, Value> {
    let mut facts = serde_json::Map::new();
    for key in [
        "files_written",
        "files_unchanged",
        "files_removed",
        "dirs_removed",
    ] {
        if let Some(v) = raw.get(key).and_then(Value::as_u64) {
            facts.insert(key.into(), json!(v));
        }
    }
    for key in ["hold_kept", "modified"] {
        if let Some(v) = raw.get(key).and_then(Value::as_bool) {
            facts.insert(key.into(), json!(v));
        }
    }
    if let Some(id) = raw
        .get("safety_snapshot_id")
        .filter(|v| v.is_null() || v.as_str().is_some_and(engine::valid_snapshot_id))
    {
        facts.insert("safety_snapshot_id".into(), id.clone());
    }
    if raw.get("mode").and_then(Value::as_str) == Some("in_place") {
        facts.insert("mode".into(), json!("in_place"));
    }
    if let Some(recovery) = raw
        .get("recovery")
        .and_then(Value::as_str)
        .filter(|v| matches!(*v, "resolved_by_user" | "superseded_by_restore"))
    {
        facts.insert("recovery".into(), json!(recovery));
        if recovery == "superseded_by_restore"
            && let Some(job) = raw
                .get("job")
                .and_then(Value::as_str)
                .filter(|v| PrincipalId::parse(v).is_ok())
        {
            facts.insert("job".into(), json!(job));
        }
    }
    facts
}
fn backup_job(host: &NetworkHost, token: &str, value: &Job) -> Value {
    let mut result = backup_job_metadata(value);
    let Some(raw) = value.result.as_ref().and_then(Value::as_object) else {
        result["result"] = Value::Null;
        return result;
    };
    // In-place results have their own closed projection; never project inspection
    // paths or unrelated engine facts into an in-place job.
    if value.kind == "restore_in_place" {
        result["result"] = Value::Object(in_place_facts(raw));
        return result;
    }
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
        .filter(|id| engine::valid_snapshot_id(id))
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

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RestoreInPlace {
    profile: String,
    snapshot: String,
    paths: Vec<String>,
    #[serde(default)]
    skip_safety_snapshot: bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ConfirmRestore {
    confirmation_token: String,
}
fn confirmation_token(value: String) -> Result<ConfirmationToken, BackupError> {
    ConfirmationToken::try_from(value).map_err(|_| BackupError::ConfirmationInvalid)
}
fn restore_target(profile_name: &str, snapshot: &str, paths: &[String]) -> bool {
    profile(profile_name)
        && engine::valid_snapshot_id(snapshot)
        && paths.len() <= 64
        && paths.iter().all(|p| restore_path(p))
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
    // A policy save names the revision it replaces, so a retry cannot apply twice;
    // it is not replayed either (a retried success reports the revision conflict).
    if matches!(route, Route::Policy | Route::PolicyReset) {
        return mutate_policy(host, &subject, route, request.method, request.bytes);
    }
    // The body is the export itself (and may carry a passphrase): never replayed.
    if matches!(route, Route::RecoveryExport) {
        if request.method != Method::POST {
            return invalid();
        }
        return recovery_export(host, &subject, request.bytes);
    }
    // Never cache/replay these responses, especially prepare's one-time secret.
    // Keys remain required and validated, but a retry prepares a fresh job/token.
    if matches!(
        route,
        Route::RestoreInPlace | Route::ConfirmRestore(_) | Route::ResolveRecovery(_)
    ) {
        if request.method != Method::POST {
            return invalid();
        }
        return mutate_restore(host, &subject, route, request.bytes);
    }
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
        Route::WorkflowRuns(_) | Route::Runs => {
            let Some(parts) = query_parts(query) else {
                return invalid();
            };
            let newest = parts
                .iter()
                .any(|(key, value)| *key == "order" && *value == "newest");
            let paging = parts
                .iter()
                .filter(|(key, _)| *key != "order")
                .map(|(k, v)| format!("{k}={v}"))
                .collect::<Vec<_>>()
                .join("&");
            if parts
                .iter()
                .any(|(key, value)| *key == "order" && *value != "newest")
            {
                return invalid();
            }
            let Some((after, limit)) = id_page(if paging.is_empty() {
                None
            } else {
                Some(&paging)
            }) else {
                return invalid();
            };
            let workflow_id = match route {
                Route::WorkflowRuns(id) => Some(id),
                _ => None,
            };
            (if newest { automation.list_runs_newest(subject,workflow_id,after,limit) } else { automation.list_runs(subject,workflow_id,after,limit) }).map(|v|json!({"runs":v.iter().map(|v|run(v,false)).collect::<Vec<_>>(),"next_after":if v.len()==limit {v.last().map(|v|v.id.as_str())} else {None}}))
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
        Route::Backups
        | Route::BackupJob(_)
        | Route::Snapshots
        | Route::Snapshot(_)
        | Route::SafetySnapshots
        | Route::Storage
        | Route::Policy => {
            return get_backups(host, token, route, query);
        }
        _ => return invalid(),
    };
    match result {
        Ok(v) => ok(v),
        Err(e) => control_failure(e, false),
    }
}
struct SnapshotPage<'a> {
    profile: &'a str,
    before: Option<(String, String)>,
    limit: usize,
}
fn snapshot_cursor(server: &str, profile: &str, safety: bool, time: &str, id: &str) -> String {
    URL_SAFE_NO_PAD
        .encode(serde_json::to_vec(&(server, profile, safety, time, id)).expect("snapshot cursor"))
}
fn snapshot_page<'a>(
    server: &str,
    safety: bool,
    query: Option<&'a str>,
) -> Option<SnapshotPage<'a>> {
    let (mut profile_name, mut before, mut limit) = (None, None, 50);
    for (key, value) in query_parts(query)? {
        match key {
            "profile" if profile(value) => profile_name = Some(value),
            "limit" => limit = usize::try_from(number(value)?).ok()?,
            "before" => before = Some(value),
            _ => return None,
        }
    }
    let profile = profile_name?;
    if !(1..=100).contains(&limit) {
        return None;
    }
    let before = if let Some(value) = before {
        Some({
            let decoded = URL_SAFE_NO_PAD.decode(value).ok()?;
            let (bound_server, bound_profile, bound_safety, time, id): (
                String,
                String,
                bool,
                String,
                String,
            ) = serde_json::from_slice(&decoded).ok()?;
            (bound_server == server
                && bound_profile == profile
                && bound_safety == safety
                && snapshot_time(&time)
                && engine::valid_snapshot_id(&id))
            .then_some((time, id))
        }?)
    } else {
        None
    };
    Some(SnapshotPage {
        profile,
        before,
        limit,
    })
}
fn valid_snapshots(snapshots: &[engine::Snapshot]) -> bool {
    snapshots.iter().all(|v| {
        engine::valid_snapshot_id(&v.id)
            && snapshot_time(&v.time)
            && v.tags.len() <= 32
            && v.tags.iter().all(|t| {
                !t.is_empty()
                    && t.len() <= 80
                    && t.bytes().all(|b| {
                        b.is_ascii_alphanumeric() || matches!(b, b'=' | b'-' | b'_' | b'.')
                    })
            })
    })
}
fn snapshot_projection(
    server: &str,
    profile: &str,
    safety: bool,
    mut snapshots: Vec<engine::Snapshot>,
    before: Option<&(String, String)>,
    limit: usize,
) -> Result<Value, BackupError> {
    if !valid_snapshots(&snapshots) {
        return Err(BackupError::Unavailable);
    }
    snapshots.sort_by(|a, b| b.time.cmp(&a.time).then_with(|| b.id.cmp(&a.id)));
    if let Some((time, id)) = before {
        snapshots.retain(|v| (&v.time, &v.id) < (time, id));
    }
    let more = snapshots.len() > limit;
    snapshots.truncate(limit);
    Ok(
        json!({"profile":profile,"snapshots":snapshots.iter().map(|v|json!({"id":v.id,"time":v.time})).collect::<Vec<_>>(),"next_before":if more {snapshots.last().map(|v|snapshot_cursor(server,profile,safety,&v.time,&v.id))} else {None}}),
    )
}
fn snapshot_detail(
    profile: &str,
    id: &str,
    snapshots: Vec<engine::Snapshot>,
) -> Result<Value, BackupError> {
    if !valid_snapshots(&snapshots) {
        return Err(BackupError::Unavailable);
    }
    Ok(snapshots.into_iter().find(|v|v.id==id).map(|v|json!({"profile":profile,"snapshot":{"id":v.id,"time":v.time,"size_bytes":null,"added_bytes":null,"file_count":null,"tags":v.tags}})).unwrap_or(Value::Null))
}
fn get_backups(host: &NetworkHost, token: &str, route: Route<'_>, query: Option<&str>) -> Reply {
    enum Query<'a> {
        Jobs(Option<(i64, String)>, usize),
        Job(&'a str),
        Snapshots(&'a str, bool, Option<(String, String)>, usize),
        Snapshot(&'a str, &'a str),
        Storage,
        Policy,
    }
    let query = match route {
        Route::Backups => {
            let Some((before, limit)) = backup_page(query) else {
                return invalid();
            };
            Query::Jobs(before, limit)
        }
        Route::BackupJob(id) if query.is_none() && PrincipalId::parse(id).is_ok() => Query::Job(id),
        Route::Snapshots | Route::SafetySnapshots => {
            let Some(page) = snapshot_page(
                host.management().server_id(),
                matches!(route, Route::SafetySnapshots),
                query,
            ) else {
                return invalid();
            };
            Query::Snapshots(
                page.profile,
                matches!(route, Route::SafetySnapshots),
                page.before,
                page.limit,
            )
        }
        Route::Snapshot(id) if engine::valid_snapshot_id(id) => {
            let Some(parts) = query_parts(query) else {
                return invalid();
            };
            let [("profile", value)] = parts.as_slice() else {
                return invalid();
            };
            if !profile(value) {
                return invalid();
            }
            Query::Snapshot(value, id)
        }
        Route::Storage if query.is_none() => Query::Storage,
        Route::Policy if query.is_none() => Query::Policy,
        _ => return invalid(),
    };
    let Some(backups) = host.backups() else {
        return backup_failure(BackupError::Disabled);
    };
    // Both list the repository under the same single-listing bound.
    let snapshot_listing = matches!(
        &query,
        Query::Snapshots(..) | Query::Snapshot(..) | Query::Storage
    );
    let result=match query {
        Query::Jobs(before,limit)=>backups.jobs(before.as_ref().map(|(timestamp,id)|(*timestamp,id.as_str())),limit).map(|jobs| {
            let profiles=backups.policy().profiles.iter().map(|p|json!({"name":p.name,"mode":match p.mode {BackupMode::Online=>"online",BackupMode::Stopped=>"stopped"},"keep_last":p.keep_last,"keep_daily":p.keep_daily,"keep_weekly":p.keep_weekly,"keep_monthly":p.keep_monthly,"on_mutation":match p.on_mutation {MutationPolicy::Warn=>"warn",MutationPolicy::FailWorld=>"fail-world",MutationPolicy::FailAny=>"fail-any"},"warn_seconds":p.warn_seconds,"max_snapshots":p.max_snapshots})).collect::<Vec<_>>();
            json!({"profiles":profiles,"jobs":jobs.iter().map(|v|backup_job(host,token,v)).collect::<Vec<_>>(),"next_before":if jobs.len()==limit {jobs.last().map(|v|backup_cursor(v.created_at,&v.id))} else {None},"restore_mode":"inspection_only","recovery_required":backups.recovery_required()})
        }),
        Query::Job(id)=>match backups.job(id) {Ok(Some(job))=> {
            let mut data = json!({"job":backup_job(host,token,&job)});
            if job.kind != "restore_in_place" { data["restore_mode"] = json!("inspection_only"); }
            Ok(data)
        },Ok(None)=>Err(BackupError::UnknownJob),Err(e)=>Err(e)},
        Query::Snapshots(profile,safety,before,limit)=>(if safety {backups.safety_snapshots(profile)} else {backups.snapshots(profile)}).and_then(|snapshots| {
            snapshot_projection(host.management().server_id(),profile,safety,snapshots,before.as_ref(),limit)
        }),
        Query::Snapshot(profile,id)=>backups.snapshots(profile).and_then(|snapshots|snapshot_detail(profile,id,snapshots)),
        Query::Storage => backups.storage().map(|status| storage_json(&status)),
        Query::Policy => Ok(policy_json(&backups.policy())),
    };
    match result {
        Ok(Value::Null) => error(StatusCode::NOT_FOUND, "resource_not_found"),
        Ok(v) => ok(v),
        Err(e) => backup_read_failure(e, snapshot_listing),
    }
}
/// Usage against budgets. `backups_stopped` carries only fixed limit codes.
fn storage_json(status: &vesta_control::backups::StorageStatus) -> serde_json::Value {
    use vesta_control::backups::{DISK_SPACE_LOW, SNAPSHOT_LIMIT, STORAGE_LIMIT};
    let stopped = status.backups_stopped.as_deref().and_then(|code| {
        [SNAPSHOT_LIMIT, STORAGE_LIMIT, DISK_SPACE_LOW]
            .into_iter()
            .find(|c| *c == code)
    });
    json!({
        "repository_bytes": status.repository_bytes,
        "restore_points": status.restore_points,
        "safety_snapshots": status.safety_snapshots,
        "free_bytes": status.free_bytes,
        "limits": {
            "max_snapshots": status.limits.max_snapshots,
            "max_repository_bytes": status.limits.max_repository_bytes,
            "min_free_bytes": status.limits.min_free_bytes,
        },
        "backups_stopped": stopped,
    })
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
            if !restore_target(&value.profile, &value.snapshot, &value.paths) {
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

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ExportRequest {
    /// Optional: encrypt the export with this passphrase (8 to 1024 bytes).
    #[serde(default)]
    passphrase: Option<String>,
}

/// Owner-only download of the credential-free control data: plain JSON, or the
/// encrypted format when a passphrase is given.
fn recovery_export(host: &NetworkHost, subject: &Subject, bytes: &[u8]) -> Reply {
    use vesta_control::recovery_export::{RecoveryExportError, export_for};
    let Some(request) = decode::<ExportRequest>(bytes) else {
        return invalid();
    };
    // Moved, not copied, into an erasing owner. The request body buffer itself is
    // outside this guarantee.
    let passphrase = request.passphrase.map(zeroize::Zeroizing::new);
    let passphrase = passphrase.as_ref().map(|p| p.as_bytes());
    match export_for(host.database(), subject, passphrase) {
        Ok(body) => crate::response(
            StatusCode::OK,
            body,
            if passphrase.is_some() {
                "application/octet-stream"
            } else {
                "application/json"
            },
        ),
        Err(e) => error(
            match e {
                RecoveryExportError::Forbidden => StatusCode::FORBIDDEN,
                RecoveryExportError::Invalid => StatusCode::BAD_REQUEST,
                RecoveryExportError::TooLarge => StatusCode::PAYLOAD_TOO_LARGE,
                RecoveryExportError::Unavailable => StatusCode::SERVICE_UNAVAILABLE,
            },
            e.code(),
        ),
    }
}

/// The policy in force and where it comes from.
fn policy_json(policy: &vesta_control::backups::BackupPolicy) -> serde_json::Value {
    let document = PolicyDocument::from_policy(&policy.profiles, &policy.limits);
    json!({
        "source": if policy.revision.is_some() { "stored" } else { "host_config" },
        "revision": policy.revision,
        "profiles": document.profiles,
        "limits": document.limits,
    })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PutPolicy {
    /// The revision being replaced; null while host.toml applies.
    expected_revision: Option<i64>,
    profiles: Vec<ProfileDocument>,
    #[serde(default)]
    limits: LimitsDocument,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ResetPolicy {
    expected_revision: i64,
}

fn mutate_policy(
    host: &NetworkHost,
    subject: &Subject,
    route: Route<'_>,
    method: &Method,
    bytes: &[u8],
) -> Reply {
    enum Command {
        Put(Option<i64>, PolicyDocument),
        Reset(i64),
    }
    let command = match (route, method) {
        (Route::Policy, &Method::PUT) => {
            let Some(value) = decode::<PutPolicy>(bytes) else {
                return invalid();
            };
            Command::Put(
                value.expected_revision,
                PolicyDocument {
                    profiles: value.profiles,
                    limits: value.limits,
                },
            )
        }
        (Route::PolicyReset, &Method::POST) => {
            let Some(value) = decode::<ResetPolicy>(bytes) else {
                return invalid();
            };
            Command::Reset(value.expected_revision)
        }
        _ => return invalid(),
    };
    let Some(backups) = host.backups() else {
        return backup_failure(BackupError::Disabled);
    };
    let result = match command {
        Command::Put(expected, document) => backups
            .set_policy(subject, expected, &document)
            .map(|revision| json!({"source":"stored","revision":revision})),
        Command::Reset(expected) => backups
            .reset_policy(subject, expected)
            .map(|()| json!({"source":"host_config","revision":null})),
    };
    match result {
        Ok(value) => ok(value),
        Err(e) => backup_failure(e),
    }
}

fn mutate_restore(host: &NetworkHost, subject: &Subject, route: Route<'_>, bytes: &[u8]) -> Reply {
    // Decode before service lookup, as with the existing backup mutations.
    enum Command<'a> {
        Prepare(RestoreInPlace),
        Confirm(&'a str, ConfirmationToken),
        Resolve(&'a str),
    }
    let command = match route {
        Route::RestoreInPlace => {
            let Some(value) = decode::<RestoreInPlace>(bytes)
                .filter(|v| restore_target(&v.profile, &v.snapshot, &v.paths))
            else {
                return invalid();
            };
            Command::Prepare(value)
        }
        Route::ConfirmRestore(id) if PrincipalId::parse(id).is_ok() => {
            let Some(value) = decode::<ConfirmRestore>(bytes) else {
                return invalid();
            };
            let token = match confirmation_token(value.confirmation_token) {
                Ok(token) => token,
                Err(e) => return backup_failure(e),
            };
            Command::Confirm(id, token)
        }
        Route::ResolveRecovery(id) if PrincipalId::parse(id).is_ok() => {
            if decode::<Empty>(bytes).is_none() {
                return invalid();
            }
            Command::Resolve(id)
        }
        _ => return invalid(),
    };
    let Some(backups) = host.backups() else {
        return backup_failure(BackupError::Disabled);
    };
    match command {
        Command::Prepare(value) => match backups.prepare_restore(
            subject,
            &value.profile,
            &value.snapshot,
            value.paths,
            value.skip_safety_snapshot,
        ) {
            Ok(prepared) => envelope(
                StatusCode::ACCEPTED,
                json!({"data":{
                    "job_id":prepared.job_id, "kind":"restore_in_place", "state":"awaiting_confirmation",
                    "confirmation_token":prepared.token.expose_secret(), "expires_at_ms":prepared.expires_at_ms
                }}),
            ),
            Err(e) => backup_failure(e),
        },
        Command::Confirm(id, token) => match backups.confirm_restore(subject, id, &token) {
            Ok(()) => envelope(
                StatusCode::ACCEPTED,
                json!({"data":{"job_id":id,"state":"queued"}}),
            ),
            Err(e) => backup_failure(e),
        },
        Command::Resolve(id) => match backups.resolve_recovery(subject, id) {
            Ok(()) => ok(json!({"job_id":id,"state":"failed","recovery":"resolved_by_user"})),
            Err(e) => backup_failure(e),
        },
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn snapshot_pages_are_bounded_stable_and_scope_bound() {
        let make = |n: u8, time: &str| engine::Snapshot {
            id: format!("{n:032x}"),
            time: time.into(),
            tags: vec!["profile=daily".into()],
        };
        let values = vec![
            make(1, "2026-10-22T00:00:00.000Z"),
            make(2, "2026-10-22T00:00:00.000Z"),
            make(3, "2026-10-21T00:00:00.000Z"),
        ];
        let first = snapshot_projection("server", "daily", false, values.clone(), None, 1).unwrap();
        assert_eq!(first["snapshots"][0]["id"], format!("{:032x}", 2));
        let cursor = first["next_before"].as_str().unwrap();
        let query = format!("profile=daily&limit=1&before={cursor}");
        let page = snapshot_page("server", false, Some(&query)).unwrap();
        assert!(snapshot_page("other", false, Some(&query)).is_none());
        assert!(snapshot_page("server", true, Some(&query)).is_none());
        assert!(snapshot_page("server", false, Some(&query.replace("daily", "weekly"))).is_none());
        let second = snapshot_projection(
            "server",
            "daily",
            false,
            values.clone(),
            page.before.as_ref(),
            1,
        )
        .unwrap();
        assert_eq!(second["snapshots"][0]["id"], format!("{:032x}", 1));
        let final_page = snapshot_projection(
            "server",
            "daily",
            false,
            values.clone(),
            page.before.as_ref(),
            100,
        )
        .unwrap();
        assert_eq!(final_page["snapshots"].as_array().unwrap().len(), 2);
        assert!(final_page["next_before"].is_null());
        let detail = snapshot_detail("daily", &format!("{:032x}", 1), values).unwrap();
        assert_eq!(detail["snapshot"]["tags"], json!(["profile=daily"]));
        for field in ["size_bytes", "added_bytes", "file_count"] {
            assert!(detail["snapshot"][field].is_null());
        }
        for query in [
            "profile=daily&limit=0",
            "profile=daily&limit=101",
            "profile=daily&profile=weekly",
            "profile=daily&before=bad",
            "profile=daily&offset=1",
        ] {
            assert!(snapshot_page("server", false, Some(query)).is_none());
        }
        assert!(
            snapshot_detail("daily", &"0".repeat(32), Vec::new())
                .unwrap()
                .is_null()
        );
        let mut corrupt = make(1, "2026-10-22T00:00:00.000Z");
        corrupt.tags = vec!["/private/path".into()];
        assert!(snapshot_detail("daily", &corrupt.id.clone(), vec![corrupt]).is_err());
    }
    use super::*;
    #[test]
    fn in_place_dtos_are_closed_and_safety_is_default() {
        let body =
            br#"{"profile":"daily","snapshot":"0123456789abcdef0123456789abcdef","paths":[]}"#;
        let value = decode::<RestoreInPlace>(body).unwrap();
        assert!(!value.skip_safety_snapshot);
        assert!(restore_target(
            &value.profile,
            &value.snapshot,
            &value.paths
        ));
        assert!(decode::<RestoreInPlace>(br#"{"profile":"daily","snapshot":"0123456789abcdef0123456789abcdef","paths":[],"skip_safety_snapshot":true}"#).unwrap().skip_safety_snapshot);
        for extra in ["requested_by", "confirmation_token", "unknown"] {
            let mut raw: Value = serde_json::from_slice(body).unwrap();
            raw[extra] = json!(true);
            assert!(decode::<RestoreInPlace>(&serde_json::to_vec(&raw).unwrap()).is_none());
        }
        assert!(
            decode::<ConfirmRestore>(br#"{"confirmation_token":"abc","unknown":true}"#).is_none()
        );
        assert!(decode::<ConfirmRestore>(br#"{"confirmation_token":1}"#).is_none());
        assert!(decode::<ConfirmRestore>(br#"{}"#).is_none());
        assert!(decode::<Empty>(br#"{"confirmation_token":"abc"}"#).is_none());
        assert!(decode::<Empty>(br#"{}"#).is_some());
        let snapshot = "0123456789abcdef0123456789abcdef";
        assert!(!restore_target("daily", "ABCDEF", &[]));
        assert!(!restore_target("-daily", snapshot, &[]));
        assert!(!restore_target("daily", snapshot, &["world/*".into()]));
        assert!(!restore_target(
            "daily",
            snapshot,
            &vec!["world".into(); 65]
        ));
        assert!(restore_target("daily", snapshot, &vec!["world".into(); 64]));
    }
    #[test]
    fn malformed_confirmation_has_the_fixed_conflict_envelope() {
        for text in [
            "".to_owned(),
            "bad token".into(),
            "a".repeat(129),
            "é".into(),
        ] {
            let reply = backup_failure(confirmation_token(text).unwrap_err());
            assert_eq!(reply.status(), StatusCode::CONFLICT);
            let crate::ReplyBody::Bytes(bytes) = reply.into_body() else {
                panic!("expected bytes");
            };
            let body: Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(body["error"]["code"], "backup_confirmation_invalid");
        }
        assert!(confirmation_token("A".repeat(43)).is_ok());
    }
    #[test]
    fn in_place_routes_do_not_collide_with_job_resources() {
        let root = "/api/v3/manage/backups";
        assert!(matches!(
            route(&format!("{root}/restore-in-place")),
            Some(Route::RestoreInPlace)
        ));
        assert!(matches!(
            route(&format!("{root}/safety-snapshots")),
            Some(Route::SafetySnapshots)
        ));
        for id in ["job-1", "12345678-1234-1234-1234-123456789abc"] {
            assert!(
                matches!(route(&format!("{root}/jobs/{id}")), Some(Route::BackupJob(v)) if v == id)
            );
            assert!(
                matches!(route(&format!("{root}/jobs/{id}/confirm")), Some(Route::ConfirmRestore(v)) if v == id)
            );
            assert!(
                matches!(route(&format!("{root}/jobs/{id}/resolve-recovery")), Some(Route::ResolveRecovery(v)) if v == id)
            );
        }
        for suffix in [
            "/confirm",
            "job/extra/confirm",
            "job/confirm/extra",
            "job/resolve-recovery/extra",
        ] {
            assert!(route(&format!("{root}/jobs/{suffix}")).is_none());
        }
        assert!(route(&format!("{root}/jobs/{}/confirm", "a".repeat(129))).is_none());
        for r in [
            Route::RestoreInPlace,
            Route::ConfirmRestore("job"),
            Route::ResolveRecovery("job"),
        ] {
            assert_eq!(capability(r, &Method::POST), Capability::BackupsRestore);
        }
        assert_eq!(
            capability(Route::SafetySnapshots, &Method::GET),
            Capability::BackupsView
        );
    }
    #[test]
    fn in_place_projection_is_typed_closed_and_secret_free() {
        use vesta_control::backups::JobState;
        let job = Job {
            id: "job-1".into(),
            profile: "daily".into(),
            kind: "restore_in_place".into(),
            state: JobState::AwaitingConfirmation,
            requested_by: "owner".into(),
            snapshot_id: None,
            result: None,
            error_code: Some("backup_confirmation_expired".into()),
            created_at: 1,
            started_at: None,
            finished_at: None,
        };
        assert_eq!(backup_job_metadata(&job)["state"], "awaiting_confirmation");
        assert_eq!(backup_job_metadata(&job)["kind"], "restore_in_place");
        assert_eq!(
            backup_job_metadata(&job)["error_code"],
            "backup_confirmation_expired"
        );
        let mut recovery_job = job;
        recovery_job.state = JobState::RecoveryRequired;
        assert_eq!(
            backup_job_metadata(&recovery_job)["state"],
            "recovery_required"
        );
        let success = json!({"mode":"in_place","safety_snapshot_id":"0123456789abcdef0123456789abcdef","files_written":1,"files_unchanged":2,"files_removed":3,"dirs_removed":4,"hold_kept":false});
        let mut raw = success.clone();
        raw["confirmation_token"] = json!("secret");
        raw["restored_to"] = json!("private/path");
        raw["unexpected"] = json!(true);
        assert_eq!(
            Value::Object(in_place_facts(raw.as_object().unwrap())),
            success
        );
        for expected in [
            json!({"mode":"in_place","safety_snapshot_id":null,"modified":true}),
            json!({"recovery":"resolved_by_user"}),
            json!({"recovery":"superseded_by_restore","job":"job-next"}),
        ] {
            assert_eq!(
                Value::Object(in_place_facts(expected.as_object().unwrap())),
                expected
            );
        }
        let invalid = json!({"mode":"inspection_only","safety_snapshot_id":"ABCDEF0123456789abcdef0123456789ab","files_written":"1","files_unchanged":-1,"files_removed":1.5,"dirs_removed":null,"hold_kept":1,"modified":"true","recovery":"arbitrary","job":"../../private","confirmation_token":"secret"});
        assert!(in_place_facts(invalid.as_object().unwrap()).is_empty());
        let invalid_job = json!({"recovery":"superseded_by_restore","job":"../secret"});
        assert_eq!(
            Value::Object(in_place_facts(invalid_job.as_object().unwrap())),
            json!({"recovery":"superseded_by_restore"})
        );
        for code in [
            "backup_confirmation_invalid",
            "backup_confirmation_expired",
            "backup_snapshot_not_restorable",
            "backup_snapshot_unknown",
            "backup_restore_path_not_in_snapshot",
            "backup_restore_target_unsafe",
            "backup_restore_restart_pending",
            "backup_safety_snapshot_failed",
            "backup_restore_interrupted",
            "backup_requires_stopped_server",
        ] {
            assert_eq!(safe_backup_code(code), code);
        }
    }
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

    #[test]
    fn storage_route_reports_budgets_and_only_fixed_stop_codes() {
        assert!(matches!(
            route("/api/v3/manage/backups/storage"),
            Some(Route::Storage)
        ));
        assert_eq!(
            capability(Route::Storage, &Method::GET),
            Capability::BackupsView
        );
        let mut status = vesta_control::backups::StorageStatus {
            repository_bytes: 10,
            restore_points: 2,
            safety_snapshots: 1,
            free_bytes: Some(5),
            limits: vesta_control::config::BackupLimits::default(),
            backups_stopped: Some("backup_storage_limit_reached".into()),
        };
        let value = storage_json(&status);
        assert_eq!(value["backups_stopped"], "backup_storage_limit_reached");
        assert_eq!(value["limits"]["max_repository_bytes"], Value::Null);
        status.backups_stopped = Some("private/path".into());
        assert_eq!(storage_json(&status)["backups_stopped"], Value::Null);
        assert_eq!(
            safe_backup_code("backup_disk_space_low"),
            "backup_disk_space_low"
        );
    }

    #[test]
    fn policy_routes_need_configure_to_change_and_refuse_unknown_fields() {
        assert!(matches!(
            route("/api/v3/manage/backups/policy"),
            Some(Route::Policy)
        ));
        assert!(matches!(
            route("/api/v3/manage/backups/policy/reset"),
            Some(Route::PolicyReset)
        ));
        assert_eq!(
            capability(Route::Policy, &Method::GET),
            Capability::BackupsView
        );
        assert_eq!(
            capability(Route::Policy, &Method::PUT),
            Capability::BackupsConfigure
        );
        assert_eq!(
            capability(Route::PolicyReset, &Method::POST),
            Capability::BackupsConfigure
        );
        let body = br#"{"expected_revision":null,"profiles":[{"name":"daily","mode":"stopped","keep_last":7}]}"#;
        let value = decode::<PutPolicy>(body).unwrap();
        assert_eq!(value.expected_revision, None);
        assert_eq!(value.limits, LimitsDocument::default());
        for bad in [
            &br#"{"expected_revision":1,"profiles":[],"repository":"C:/x"}"#[..],
            br#"{"expected_revision":1,"profiles":[{"name":"d","mode":"stopped","path":"x"}]}"#,
            br#"{"expected_revision":1,"profiles":[],"limits":{"max_bytes":1}}"#,
        ] {
            assert!(decode::<PutPolicy>(bad).is_none());
        }
        assert!(decode::<ResetPolicy>(br#"{}"#).is_none());
        assert!(decode::<ResetPolicy>(br#"{"expected_revision":3}"#).is_some());
        let host = vesta_control::backups::BackupPolicy {
            profiles: vec![],
            limits: vesta_control::config::BackupLimits::default(),
            revision: None,
        };
        assert_eq!(policy_json(&host)["source"], "host_config");
        assert_eq!(policy_json(&host)["limits"]["min_free_bytes"], 1u64 << 30);
        assert_eq!(
            backup_failure(BackupError::PolicyConflict).status(),
            StatusCode::CONFLICT
        );
    }

    #[test]
    fn recovery_export_is_owner_only_and_takes_only_an_optional_passphrase() {
        assert!(matches!(
            route("/api/v3/manage/recovery/export"),
            Some(Route::RecoveryExport)
        ));
        assert_eq!(
            capability(Route::RecoveryExport, &Method::POST),
            Capability::RecoveryExport
        );
        assert!(Capability::RecoveryExport.owner_reserved());
        assert!(
            decode::<ExportRequest>(br#"{}"#)
                .unwrap()
                .passphrase
                .is_none()
        );
        assert_eq!(
            decode::<ExportRequest>(br#"{"passphrase":"correct horse"}"#)
                .unwrap()
                .passphrase
                .as_deref(),
            Some("correct horse")
        );
        for bad in [
            &br#"{"passphrase":1}"#[..],
            br#"{"passphrase":"x","tables":["pack_passwords"]}"#,
            br#"{"output":"C:/x"}"#,
        ] {
            assert!(decode::<ExportRequest>(bad).is_none());
        }
    }
}
