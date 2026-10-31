//! Authorized settings and non-authoritative previews for the offline responder.
use super::*;
use vesta_control::{
    motd::{Document, PublicInputs, State},
    sleep::Policy,
};
pub(crate) fn implemented(path: &str) -> bool {
    matches!(
        path,
        "/api/v3/manage/sleep/policy"
            | "/api/v3/manage/motd/policy"
            | "/api/v3/manage/motd/preview"
    )
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SleepPut {
    expected_revision: i64,
    policy: Policy,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MotdPut {
    expected_revision: Option<i64>,
    document: Document,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Preview {
    state: State,
    document: Document,
}
pub(super) fn dispatch(host: &NetworkHost, request: Request<'_>) -> Reply {
    let preview = request.path.ends_with("/preview");
    let mutation = request.method == Method::PUT;
    if request.query.is_some()
        || request.if_match.is_some()
        || request.if_none_match.is_some()
        || request.if_range.is_some()
        || request.range.is_some()
        || !((preview && request.method == Method::POST)
            || (!preview && (request.method == Method::GET || mutation)))
        || (request.method == Method::GET && !request.bytes.is_empty())
    {
        return error(StatusCode::BAD_REQUEST, "invalid_request");
    }
    let Some(token) = request.authorization else {
        return error(StatusCode::UNAUTHORIZED, "management_session_invalid");
    };
    let subject = match host.management().authorize(
        token,
        if mutation {
            Capability::ConfigEdit
        } else {
            Capability::ConfigView
        },
        None,
    ) {
        Ok(v) => v,
        Err(e) => return management_error(e),
    };
    if request.path.contains("/sleep/") {
        let result = if mutation {
            let Some(value) = decode::<SleepPut>(request.bytes) else {
                return error(StatusCode::BAD_REQUEST, "invalid_request");
            };
            host.sleep()
                .configure(&subject, value.expected_revision, value.policy)
        } else {
            host.sleep().policy(&subject)
        };
        return match result {
            Ok(v) => ok(json!(v)),
            Err("sleep_forbidden") => error(StatusCode::FORBIDDEN, "forbidden"),
            Err("sleep_policy_stale_revision") => {
                match host.sleep().policy(&subject) {
                    Ok(current) => envelope(StatusCode::CONFLICT, json!({"error":{"code":"conflict","reason":"sleep_policy_conflict","current":current}})),
                    Err(code) => error(StatusCode::SERVICE_UNAVAILABLE, code),
                }
            }
            Err("sleep_policy_invalid") => error(StatusCode::BAD_REQUEST, "invalid_request"),
            Err(code) => error(StatusCode::SERVICE_UNAVAILABLE, code),
        };
    }
    let operations = host.runtime().operations_service();
    if mutation {
        let Some(value) = decode::<MotdPut>(request.bytes) else {
            return error(StatusCode::BAD_REQUEST, "invalid_request");
        };
        return match operations.set_motd_policy(&subject, value.expected_revision, value.document) {
            Ok(revision) => ok(
                json!({"revision":revision,"offline_apply":"immediate","live_apply":"separate_server_properties_edit_required"}),
            ),
            Err(OperationError::Rejected {
                code: OperationErrorCode::SourceRevisionUnavailable,
                ..
            }) => match operations.motd_policy(&subject) {
                Ok(current) => envelope(StatusCode::CONFLICT, json!({"error":{"code":"conflict","reason":"motd_policy_conflict","current":current}})),
                Err(e) => operation_error(e),
            },
            Err(e) => operation_error(e),
        };
    }
    if preview {
        let Some(value) = decode::<Preview>(request.bytes) else {
            return error(StatusCode::BAD_REQUEST, "invalid_request");
        };
        // Preview uses public configuration examples, never administrative errors,
        // private paths or caller-supplied diagnostic/operation text.
        let pack = host.configuration().pack();
        let inputs = PublicInputs {
            server_name: pack.name(),
            minecraft_version: pack.minecraft_version(),
            pack_name: pack.name(),
            pack_version: pack.version(),
            ..PublicInputs::default()
        };
        return match value.document.render(value.state, &inputs) {
            Ok(v) => ok(
                json!({"preview":v,"placeholders":vesta_control::motd::PLACEHOLDERS,"live_apply":"separate_server_properties_edit_required"}),
            ),
            Err(_) => error(StatusCode::BAD_REQUEST, "invalid_request"),
        };
    }
    match operations.motd_policy(&subject) {
        Ok(v) => ok(json!(v)),
        Err(e) => operation_error(e),
    }
}
