use super::*;
use vesta_control::{
    identity::Capability,
    machine::{
        HostState,
        startup::{StartupAction, StartupSettings},
    },
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Patch {
    expected_version: String,
    on_agent_start: Option<StartupAction>,
    start_delay_seconds: Option<u32>,
}

pub(super) fn dispatch(
    state: &HostState,
    device: &Device,
    server: Uuid,
    worker: Option<&dyn ServerWorker>,
    method: &Method,
    query: Option<&str>,
    bytes: &[u8],
) -> Reply {
    fn response(
        state: &HostState,
        device: &Device,
        server: Uuid,
        worker: Option<&dyn ServerWorker>,
        method: &Method,
        query: Option<&str>,
        bytes: &[u8],
    ) -> Result<Reply, Reply> {
        let error = super::access::access_error;
        if query.is_some() {
            return Err(failure(StatusCode::BAD_REQUEST, "invalid_request"));
        }
        let fallback = StartupSettings {
            on_agent_start: worker
                .map(ServerWorker::configured_startup)
                .unwrap_or_default(),
            start_delay_seconds: 0,
        };
        let (revision, settings) = match method.as_str() {
            "GET" if bytes.is_empty() => {
                let role = state
                    .effective_role(device.account_id, server)
                    .map_err(error)?
                    .ok_or_else(|| failure(StatusCode::NOT_FOUND, "not_found"))?;
                let caps = state
                    .server_capabilities(server, device.account_id, role)
                    .map_err(error)?;
                if !caps.contains(&Capability::StatusView) {
                    return Err(failure(StatusCode::FORBIDDEN, "management_forbidden"));
                }
                (
                    state.snapshot().map_err(error)?.revision,
                    state
                        .startup_settings(server)
                        .map_err(error)?
                        .unwrap_or(fallback),
                )
            }
            "PATCH" => {
                let input: Patch = parse(bytes)?;
                state
                    .patch_startup(
                        Actor::Account(device.account_id),
                        server,
                        super::access::version(&input.expected_version)?,
                        fallback,
                        input.on_agent_start,
                        input.start_delay_seconds,
                    )
                    .map_err(error)?
            }
            _ => return Err(failure(StatusCode::BAD_REQUEST, "invalid_request")),
        };
        let last_startup = worker
            .and_then(ServerWorker::startup_report)
            .into_iter()
            .chain(state.startup_open_failure(server).map_err(error)?)
            .max_by_key(|report| report.at_ms);
        Ok(crate::manage::ok(json!({
            "version":revision.to_string(),
            "on_agent_start":settings.on_agent_start,
            "start_delay_seconds":settings.start_delay_seconds,
            "enabled":state.server(server).map_err(error)?.is_some_and(|entry| entry.state != vesta_control::machine::registry::ServerState::Disabled),
            "last_startup":last_startup,
        })))
    }
    response(state, device, server, worker, method, query, bytes).unwrap_or_else(|error| error)
}
