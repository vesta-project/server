//! One bounded status projection shared by HTTP validators and metrics-stream status.
use super::*;
use sha2::{Digest, Sha256};
use vesta_control::operations::ProcessPhase;

pub(crate) fn projection(host: &NetworkHost) -> Result<Value, Box<Reply>> {
    let runtime = host.runtime();
    let (snapshot, uptime) = match runtime.game_observation() {
        Ok(value) => value,
        Err(e) => return Err(Box::new(operation_error(e))),
    };
    let phase = match snapshot.phase {
        ProcessPhase::Stopped => "stopped",
        ProcessPhase::Running => "running",
        ProcessPhase::Stopping => "stopping",
        ProcessPhase::Draining => "draining",
    };
    let count = runtime.last_player_count(snapshot.generation);
    let supervision = match runtime.supervision() {
        Ok(value) => value,
        Err(e) => return Err(Box::new(operation_error(e))),
    };
    let readiness = match runtime.readiness() {
        Ok(value) => value,
        Err(e) => return Err(Box::new(operation_error(e))),
    };
    use vesta_control::operations::readiness::{DegradedReason, Readiness};
    let (ready_state, ready_reason) = match readiness.readiness {
        Readiness::Stopped => ("stopped", None),
        Readiness::Starting => ("starting", None),
        Readiness::Ready => ("ready", None),
        Readiness::Degraded(reason) => (
            "degraded",
            Some(match reason {
                DegradedReason::StartupTimeout => "startup_timeout",
                DegradedReason::ProbeFailed => "probe_failed",
                DegradedReason::FatalOutput => "fatal_output",
                DegradedReason::ConsoleGap => "console_gap",
            }),
        ),
    };
    let supervisor = supervision.map(|h| {
        json!({"orphan_running":h.orphan_running,"launch_unresolved":h.launch_unresolved,"observation_failed":h.observation_failed,"crash_restart_enabled":h.restart_enabled,"crash_loop":h.crash_loop,"restart_state_unsaved":h.restart_state_unsaved,"recent_crashes":h.recent_crashes,"restart_pending_ms":h.restart_pending_ms.map(|v| v.to_string())})
    });
    let lifecycle = match host.sleep().public_state(runtime) {
        Ok(v) => v,
        Err(code) => return Err(Box::new(error(StatusCode::SERVICE_UNAVAILABLE, code))),
    };
    Ok(
        json!({"state":phase,"lifecycle":lifecycle,"sleep_available":host.sleep().available(),"on_agent_start":match host.configuration().on_agent_start() { vesta_control::config::OnAgentStart::StayStopped => "stay_stopped", vesta_control::config::OnAgentStart::Sleep => "sleep", vesta_control::config::OnAgentStart::Start => "start" },"sleep_blocker":host.sleep().sleep_blocker().map_err(|code| Box::new(error(StatusCode::SERVICE_UNAVAILABLE, code)))?,"generation":snapshot.generation.map(GenerationId::to_hex),"player_count":count.map(|v| v.0),"player_count_known":count.is_some(),"player_roster_revision":runtime.player_roster_revision(snapshot.generation).map(|v| v.to_string()),"player_count_observed_at_ms":count.map(|v| v.1),"uptime_seconds":uptime.map(|v| v.to_string()),"supervision":if supervision.is_some() {"separate"} else {"in_process"},"supervisor":supervisor,"readiness":ready_state,"readiness_reason":ready_reason,"readiness_evidence":if readiness.probed {"log_and_probe"} else {"log_only"}}),
    )
}
pub(crate) fn tag(data: &Value) -> String {
    format!(
        "\"status-{:x}\"",
        Sha256::digest(serde_json::to_vec(data).expect("status JSON"))
    )
}
fn tagged(mut reply: Reply, tag: &str) -> Reply {
    reply
        .headers_mut()
        .insert("etag", tag.parse().expect("digest validator"));
    reply
}
pub(super) fn dispatch(host: &NetworkHost, request: Request<'_>) -> Reply {
    if request.query.is_some() || !request.bytes.is_empty() || request.method != Method::GET {
        return error(StatusCode::BAD_REQUEST, "invalid_request");
    }
    let Some(token) = request.authorization else {
        return error(StatusCode::UNAUTHORIZED, "management_session_invalid");
    };
    if let Err(e) = host
        .management()
        .authorize(token, Capability::StatusView, None)
    {
        return management_error(e);
    }
    // Same single-strong-validator subset as metrics/current.
    if request.if_none_match.is_some_and(|v| {
        v.len() < 2
            || v.len() > 128
            || !v.starts_with('"')
            || !v.ends_with('"')
            || !v[1..v.len() - 1]
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
    }) {
        return error(StatusCode::BAD_REQUEST, "invalid_request");
    }
    let data = match projection(host) {
        Ok(v) => v,
        Err(reply) => return *reply,
    };
    let tag = tag(&data);
    if request.if_none_match == Some(tag.as_str()) {
        return tagged(
            http::Response::builder()
                .status(StatusCode::NOT_MODIFIED)
                .header("cache-control", "no-store")
                .body(crate::ReplyBody::Bytes(hyper::body::Bytes::new()))
                .expect("fixed response"),
            &tag,
        );
    }
    // Stable representation metadata makes the data-derived validator strong:
    // equal tags identify byte-identical envelopes, not changing request counters.
    tagged(
        response(
            StatusCode::OK,
            serde_json::to_vec(
                &json!({"api_version":"3","request_id":tag.trim_matches('"'),"data":data}),
            )
            .expect("JSON"),
            "application/json",
        ),
        &tag,
    )
}
