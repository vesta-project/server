use super::access::{access_error, version};
use super::*;
use vesta_control::machine::registry::Visibility;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Settings {
    expected_version: String,
    default_role_id: Option<String>,
    visibility: Visibility,
}

pub(super) fn settings(
    router: &HostRouter,
    device: &Device,
    server: Uuid,
    method: &Method,
    query: Option<&str>,
    bytes: &[u8],
) -> Reply {
    let state = &router.identity.state;
    let actor = Actor::Account(device.account_id);
    let result = (|| {
        if query.is_some() {
            return Err(failure(StatusCode::BAD_REQUEST, "invalid_request"));
        }
        if method == Method::GET && bytes.is_empty() {
            return state
                .server_default_access(actor, server)
                .map_err(access_error);
        }
        if method != Method::PUT {
            return Err(failure(StatusCode::NOT_FOUND, "not_found"));
        }
        let value: serde_json::Value = parse(bytes)?;
        if value.get("default_role_id").is_none() {
            return Err(failure(StatusCode::BAD_REQUEST, "invalid_request"));
        }
        let input: Settings = parse(bytes)?;
        let value = state
            .set_server_default_access(
                actor,
                server,
                version(&input.expected_version)?,
                input.default_role_id.as_deref(),
                input.visibility,
            )
            .map_err(access_error)?;
        router.refresh_worker_policies(Some(server))?;
        Ok(value)
    })();
    result.map_or_else(|reply| reply, crate::manage::ok)
}

pub(super) fn here(app: &Application, query: Option<&str>, peer: SocketAddr) -> Reply {
    let result = (|| {
        let decision = app
            .here_peer_limiter
            .lock()
            .map_or(AdmissionDecision::ClockUnavailable, |mut limiter| {
                limiter.try_acquire(peer.ip())
            });
        Application::check_rate(decision)?;
        let decision = app
            .here_host_limiter
            .lock()
            .map_or(AdmissionDecision::ClockUnavailable, |mut limiter| {
                limiter.try_acquire_key(())
            });
        Application::check_rate(decision)?;
        let port = query
            .and_then(|q| q.strip_prefix("port="))
            .filter(|value| value.len() <= 5 && value.bytes().all(|b| b.is_ascii_digit()))
            .and_then(|value| value.parse::<u16>().ok())
            .filter(|&port| port != 0)
            .ok_or_else(|| failure(StatusCode::BAD_REQUEST, "invalid_request"))?;
        let unavailable = || failure(StatusCode::SERVICE_UNAVAILABLE, "host_unavailable");
        let (host, server) = app
            .router
            .identity
            .state
            .public_server_at_port(port)
            .map_err(|_| unavailable())?
            .ok_or_else(|| failure(StatusCode::NOT_FOUND, "not_found"))?;
        let fingerprint = app.tls.fingerprint().map_err(|_| unavailable())?;
        Ok(routes::json(json!({
            "hostId":host, "serverId":server, "gamePort":port, "fingerprint":fingerprint, "pack":null,
        })))
    })();
    let mut reply = result.unwrap_or_else(|reply| reply);
    reply.headers_mut().insert(
        http::header::CACHE_CONTROL,
        http::HeaderValue::from_static("no-store"),
    );
    reply
}
