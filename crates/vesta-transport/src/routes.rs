use super::{Reply, Shared, error, rate_limited, response};
use http::{Method, Request, StatusCode};
use http_body_util::BodyExt;
use hyper::body::Incoming;
use serde::Deserialize;
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use vesta_control::{
    host::NetworkHost,
    identity::{
        PasswordVerificationStatus,
        minecraft::{ChallengeId, MinecraftName, MinecraftUuid},
    },
    packs::ArtifactFormat,
};
use vesta_protocol::discovery::{self, Failure};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Password {
    password: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Player {
    uuid: String,
    name: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Completion {
    challenge: String,
    #[serde(default)]
    device_name: Option<String>,
    #[serde(default)]
    public_key: Option<String>,
}

pub(super) fn fixed_refusal(path: &str) -> Option<Reply> {
    if path.starts_with("/api/v2/manage/") || path == "/api/v2/manage" {
        return Some(error(StatusCode::FORBIDDEN, "management_unavailable"));
    }
    if path.starts_with("/api/v1/") || path == "/api/v1" {
        return Some(error(StatusCode::GONE, "legacy_endpoint_removed"));
    }
    None
}

pub(super) async fn route(
    request: Request<Incoming>,
    state: Arc<Shared>,
    peer: std::net::SocketAddr,
) -> Reply {
    let original_path = request.uri().path();
    let auth_alias = matches!(
        original_path,
        "/api/v3/auth/device/enroll"
            | "/api/v3/auth/minecraft/challenge"
            | "/api/v3/auth/minecraft/complete"
            | "/api/v3/auth/session/challenge"
            | "/api/v3/auth/session"
            | "/api/v3/auth/password"
    );
    let path = if auth_alias {
        original_path.replacen("/api/v3/auth/", "/api/v3/manage/", 1)
    } else {
        original_path.to_owned()
    };
    let method = request.method().clone();
    let operational = super::manage::implemented(&path)
        || path == "/api/v3/manage/session" && method == Method::GET;
    let idempotency = if request.headers().get_all("idempotency-key").iter().count() == 1 {
        request
            .headers()
            .get("idempotency-key")
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned)
    } else {
        None
    };
    let conditional_header = |name: &str| -> Result<Option<String>, ()> {
        if !(super::manage::file_endpoint(&path)
            || super::manage::presentation::implemented(&path)
            || name == "if-none-match"
                && (super::manage::metrics_endpoint(&path)
                    || path == "/api/v3/manage/server/status"))
        {
            return Ok(None);
        }
        let count = request.headers().get_all(name).iter().count();
        match count {
            0 => Ok(None),
            1 => request
                .headers()
                .get(name)
                .and_then(|v| v.to_str().ok())
                .map(|v| Some(v.to_owned()))
                .ok_or(()),
            _ => Err(()),
        }
    };
    let (Ok(if_match), Ok(if_none_match), Ok(range), Ok(if_range)) = (
        conditional_header("if-match"),
        conditional_header("if-none-match"),
        conditional_header("range"),
        conditional_header("if-range"),
    ) else {
        return super::manage::error(StatusCode::BAD_REQUEST, "invalid_request");
    };
    let management = path == "/api/v3/manage" || path.starts_with("/api/v3/manage/");
    let management_public = method == Method::POST
        && matches!(
            path.as_str(),
            "/api/v3/manage/pair"
                | "/api/v3/manage/session/challenge"
                | "/api/v3/manage/session"
                | "/api/v3/manage/minecraft/challenge"
                | "/api/v3/manage/minecraft/complete"
                | "/api/v3/manage/device/enroll"
                | "/api/v3/manage/password"
        );
    let recognized = management
        || matches!(
            (method.as_str(), path.as_str()),
            (
                "POST",
                "/api/v2/discovery"
                    | "/api/v2/auth/password"
                    | "/api/v2/auth/minecraft/challenge"
                    | "/api/v2/auth/minecraft/complete"
                    | "/api/v2/pack/ticket"
                    | "/api/v2/realtime/ticket"
            ) | (
                "GET",
                "/api/v2/pack/status"
                    | "/api/v2/pack/artifact"
                    | "/api/v2/realtime"
                    | "/api/v3/status"
            )
        );
    if !recognized {
        if path == "/api/v3/status" {
            return super::manage::error(StatusCode::NOT_FOUND, "endpoint_not_found");
        }
        return error(StatusCode::NOT_FOUND, "endpoint_not_found");
    }
    if matches!(
        path.as_str(),
        "/api/v3/manage/console/stream"
            | "/api/v3/manage/files/follow"
            | "/api/v3/manage/metrics/stream"
    ) && method == Method::GET
    {
        return super::realtime::upgrade(request, state).await;
    }
    if path == "/api/v2/realtime" {
        return super::realtime::upgrade(request, state).await;
    }
    let error = |status, code| {
        if operational || path == "/api/v3/status" {
            super::manage::error(status, code)
        } else {
            error(status, code)
        }
    };
    let query = request.uri().query().map(str::to_owned);
    if !operational && path != "/api/v2/pack/status" && query.is_some() {
        return error(StatusCode::BAD_REQUEST, "invalid_request");
    }
    // Exactly one observed Authorization field; never accept query/cookie secrets.
    let authorization =
        bearer(request.headers()).map(|secret| zeroize::Zeroizing::new(secret.to_owned()));
    if management && !management_public && authorization.is_none() {
        return error(StatusCode::UNAUTHORIZED, "management_session_invalid");
    }
    if (method == Method::POST
        || (method == Method::PATCH && path == "/api/v3/manage/config/server-properties"))
        && !matches!(
            path.as_str(),
            "/api/v2/pack/ticket" | "/api/v2/realtime/ticket"
        )
        && request
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            != Some("application/json")
    {
        return error(StatusCode::BAD_REQUEST, "invalid_request");
    }
    if request.headers().contains_key("cookie") {
        return error(StatusCode::BAD_REQUEST, "invalid_request");
    }
    if ((super::manage::permission_endpoint(&path)
        && (method == Method::PUT
            || method == Method::PATCH
            || (method == Method::DELETE && path.starts_with("/api/v3/manage/roles/"))))
        || ((super::manage::automation_endpoint(&path)
            || super::manage::presentation::implemented(&path))
            && matches!(method, Method::POST | Method::PUT | Method::DELETE)))
        && (request.headers().get_all("content-type").iter().count() != 1
            || request
                .headers()
                .get("content-type")
                .and_then(|v| v.to_str().ok())
                != Some("application/json"))
    {
        return error(StatusCode::BAD_REQUEST, "invalid_request");
    }
    let maximum = state.max_body;
    if request
        .headers()
        .get("content-length")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok())
        .is_some_and(|n| n > maximum as u64)
    {
        return error(StatusCode::PAYLOAD_TOO_LARGE, "request_too_large");
    }
    let mut body = request.into_body();
    let mut bytes = zeroize::Zeroizing::new(Vec::new());
    while let Some(frame) = body.frame().await {
        let Ok(frame) = frame else {
            return error(StatusCode::BAD_REQUEST, "invalid_request");
        };
        if let Ok(data) = frame.into_data() {
            if data.len() > maximum.saturating_sub(bytes.len()) {
                return error(StatusCode::PAYLOAD_TOO_LARGE, "request_too_large");
            }
            bytes.extend_from_slice(&data);
        } else {
            return error(StatusCode::BAD_REQUEST, "invalid_request");
        }
    }
    if (method == Method::GET
        || matches!(
            path.as_str(),
            "/api/v2/pack/ticket" | "/api/v2/realtime/ticket"
        ))
        && !bytes.is_empty()
    {
        return error(StatusCode::BAD_REQUEST, "invalid_request");
    }
    state.jobs.reap(false).await;
    let host = state.host.clone();
    let downloads = Requester {
        downloads: state.downloads.clone(),
        peer: peer.ip(),
    };
    let tls = state.tls.clone();
    let jobs = if path == "/api/v3/manage/content/install" && method == Method::POST {
        state.install_jobs.reap(false).await;
        &state.install_jobs
    } else {
        &state.jobs
    };
    let version3 = operational || path == "/api/v3/status";
    jobs.execute_for(version3, move || {
        if operational {
            let fingerprint = if path == "/api/v3/manage/invitations" && method == Method::POST {
                tls.fingerprint().ok()
            } else {
                None
            };
            return super::manage::dispatch(
                &host,
                super::manage::Request {
                    tls_fingerprint: fingerprint.as_deref(),
                    method: &method,
                    path: &path,
                    query: query.as_deref(),
                    authorization: authorization.as_ref().map(|secret| secret.as_str()),
                    idempotency: idempotency.as_deref(),
                    bytes: &bytes,
                    if_match: if_match.as_deref(),
                    if_none_match: if_none_match.as_deref(),
                    range: range.as_deref(),
                    if_range: if_range.as_deref(),
                },
            );
        }
        if management {
            return manage_dispatch(
                &host,
                &path,
                management_public,
                authorization.as_ref().map(|secret| secret.as_str()),
                &bytes,
            );
        }
        dispatch(
            &host,
            &path,
            query.as_deref(),
            authorization.as_ref().map(|secret| secret.as_str()),
            &bytes,
            downloads,
            tls,
        )
    })
    .await
}

/// Download admission for one request: the shared limiter and the socket peer.
struct Requester {
    downloads: Arc<super::Downloads<vesta_control::pack_access::RealtimeSession>>,
    peer: std::net::IpAddr,
}

fn json(value: serde_json::Value) -> Reply {
    response(
        StatusCode::OK,
        serde_json::to_vec(&value).expect("JSON value encoding"),
        "application/json",
    )
}
fn dispatch(
    host: &NetworkHost,
    path: &str,
    query: Option<&str>,
    authorization: Option<&str>,
    bytes: &[u8],
    downloads: Requester,
    tls: Arc<crate::tls::Material>,
) -> Reply {
    if path == "/api/v3/status" {
        let runtime = host.runtime();
        let presentation = match host.sleep().public_presentation(runtime) {
            Ok(v) => v,
            Err(code) => return super::manage::error(StatusCode::SERVICE_UNAVAILABLE, code),
        };
        return json(
            serde_json::json!({"api_version":"3","server_id":host.management().server_id(),"state":presentation.state,"motd":presentation}),
        );
    }
    if matches!(path, "/api/v2/discovery" | "/api/v2/pack/status") {
        let admitted = host.with_policy(|p| {
            host.sessions()
                .authorize_info(authorization, Instant::now(), p)
        });
        if !matches!(admitted, Ok(Ok(()))) {
            return error(StatusCode::UNAUTHORIZED, "pack_session_invalid");
        }
    }
    match path {
        "/api/v2/discovery" => {
            let manifest = discovery::decode_request(bytes)
                .and_then(|request| host.discover(&request))
                .and_then(|manifest| discovery::encode_manifest(&manifest));
            match manifest {
                Ok(bytes) => {
                    let Ok(fingerprint) = tls.fingerprint() else {
                        return error(StatusCode::SERVICE_UNAVAILABLE, "tls_material_unavailable");
                    };
                    let mut reply = response(StatusCode::OK, bytes, "application/json");
                    // Keep the strict versioned manifest unchanged. This is descriptive
                    // leaf DER SHA-256, not a trust grant or an SPKI identity.
                    reply.headers_mut().insert(
                        "x-vesta-tls-sha256-fingerprint",
                        http::HeaderValue::from_str(&fingerprint).expect("hex fingerprint"),
                    );
                    reply
                }
                Err(e) => response(
                    StatusCode::BAD_REQUEST,
                    discovery::encode_failure(&Failure { error: e.code() })
                        .expect("fixed failure encoding"),
                    "application/json",
                ),
            }
        }
        "/api/v2/pack/status" => {
            let installed = match query
                .and_then(|q| q.strip_prefix("installed="))
                .filter(|s| !s.is_empty() && s.len() <= 20 && s.bytes().all(|b| b.is_ascii_digit()))
                .and_then(|s| s.parse::<u64>().ok())
            {
                Some(n) => n,
                None => return error(StatusCode::BAD_REQUEST, "invalid_request"),
            };
            match host.releases().current() {
                Ok(None) => json(serde_json::json!({"published": false})),
                Ok(Some(release)) => json(
                    serde_json::json!({"published": true, "name": host.configuration().pack().name(), "version": host.configuration().pack().version(), "revision": release.revision(), "minimum_revision": release.minimum_revision(), "display_version": release.display_version(), "status": release.status_for(installed).wire()}),
                ),
                Err(_) => error(StatusCode::SERVICE_UNAVAILABLE, "pack_unavailable"),
            }
        }
        "/api/v2/auth/password" => {
            let Ok(input) = serde_json::from_slice::<Password>(bytes) else {
                return error(StatusCode::BAD_REQUEST, "invalid_request");
            };
            let secret = zeroize::Zeroizing::new(input.password);
            if secret.len() > 1024 {
                return error(StatusCode::BAD_REQUEST, "invalid_request");
            }
            let Some(passwords) = host.passwords() else {
                return error(StatusCode::FORBIDDEN, "pack_access_denied");
            };
            let Ok(verification) = passwords.verify(secret.as_bytes().to_vec()) else {
                return error(StatusCode::SERVICE_UNAVAILABLE, "pack_access_unavailable");
            };
            match verification.status() {
                PasswordVerificationStatus::RateLimited => {
                    // The composed production KDF gate refills verification at
                    // one token per second; this is a retry hint, not a reservation.
                    return rate_limited("password_rate_limited", Duration::from_secs(1));
                }
                PasswordVerificationStatus::Busy => {
                    return error(StatusCode::SERVICE_UNAVAILABLE, "password_busy");
                }
                _ => {}
            }
            match host.with_policy(|policy| policy.grant_password(&verification)) {
                Ok(Some(_)) => {
                    let Some(token) = authorization else {
                        return error(StatusCode::UNAUTHORIZED, "device_session_required");
                    };
                    match host.with_policy(|policy| {
                        host.management()
                            .grant_password(token, &verification, policy)
                    }) {
                        Ok(Ok(())) => json(
                            serde_json::json!({"session":token,"expires_in":900,"granted":true}),
                        ),
                        _ => error(StatusCode::FORBIDDEN, "pack_access_denied"),
                    }
                }
                _ => error(StatusCode::FORBIDDEN, "pack_access_denied"),
            }
        }
        "/api/v2/auth/minecraft/challenge" => {
            let Ok(input) = serde_json::from_slice::<Player>(bytes) else {
                return error(StatusCode::BAD_REQUEST, "invalid_request");
            };
            let (Ok(uuid), Ok(name)) = (
                MinecraftUuid::parse(&input.uuid),
                MinecraftName::parse(&input.name),
            ) else {
                return error(StatusCode::BAD_REQUEST, "invalid_request");
            };
            match host.minecraft().issue(uuid, name) {
                Ok(challenge) => json(
                    serde_json::json!({"challenge": challenge.challenge_id().as_str(), "server_hash": challenge.server_hash().as_str(), "expires_in": challenge.lifetime().as_secs()}),
                ),
                Err(e) => minecraft_error(e.code()),
            }
        }
        "/api/v2/auth/minecraft/complete" => {
            let Ok(input) = serde_json::from_slice::<Completion>(bytes) else {
                return error(StatusCode::BAD_REQUEST, "invalid_request");
            };
            let Ok(challenge) = ChallengeId::parse(&input.challenge) else {
                return error(StatusCode::BAD_REQUEST, "invalid_request");
            };
            match host.minecraft().complete(&challenge) {
                Ok(Some(profile)) => {
                    let (Some(name), Some(key)) = (input.device_name, input.public_key) else {
                        return error(StatusCode::BAD_REQUEST, "invalid_request");
                    };
                    match host
                        .management()
                        .enroll_pack_profile(profile, &name, &key, authorization)
                    {
                        Ok(device) => json(serde_json::json!({"device_id":device.device_id})),
                        Err(e) => management_error(e),
                    }
                }
                Ok(None) => error(StatusCode::FORBIDDEN, "pack_access_denied"),
                Err(e) => minecraft_error(e.code()),
            }
        }
        "/api/v2/pack/ticket" | "/api/v2/realtime/ticket" => {
            let secret = authorization.unwrap_or("");
            match host.with_policy(|p| {
                host.sessions().issue_ticket_for(
                    secret,
                    if path == "/api/v2/realtime/ticket" {
                        vesta_control::pack_access::TicketPurpose::Realtime
                    } else {
                        vesta_control::pack_access::TicketPurpose::Download
                    },
                    Instant::now(),
                    p,
                )
            }) {
                Ok(Ok(ticket)) => json(serde_json::json!({"ticket": ticket.expose()})),
                Ok(Err(vesta_control::pack_access::PackAccessError::Invalid)) => {
                    error(StatusCode::UNAUTHORIZED, "pack_session_invalid")
                }
                _ => error(StatusCode::SERVICE_UNAVAILABLE, "pack_session_unavailable"),
            }
        }
        "/api/v2/pack/artifact" => {
            let secret = authorization.unwrap_or("");
            // A session bearer and its download tickets share one session reference.
            let Ok(Ok((reference, _))) = host.with_policy(|p| {
                host.sessions()
                    .authenticate_reference(secret, Instant::now(), p)
                    .or_else(|_| host.sessions().redeem_download(secret, Instant::now(), p))
            }) else {
                return error(StatusCode::UNAUTHORIZED, "pack_session_invalid");
            };
            let release = match host.releases().current() {
                Ok(Some(r)) => r,
                Ok(None) => return error(StatusCode::NOT_FOUND, "pack_not_published"),
                Err(_) => return error(StatusCode::SERVICE_UNAVAILABLE, "pack_unavailable"),
            };
            let Some(permit) = downloads.downloads.acquire(reference, downloads.peer) else {
                return error(StatusCode::SERVICE_UNAVAILABLE, "download_busy");
            };
            let artifact = release.artifact();
            let Ok(file) = host.releases().artifacts().open_pinned(artifact) else {
                return error(StatusCode::SERVICE_UNAVAILABLE, "pack_unavailable");
            };
            let content_type = match artifact.format() {
                ArtifactFormat::Vesta => "application/vnd.vesta.pack",
                ArtifactFormat::Catalogue => "application/vnd.vesta.catalogue+json",
                ArtifactFormat::Zip | ArtifactFormat::Mrpack => "application/zip",
            };
            let Ok(body) = super::ArtifactBody::new(file, artifact, permit) else {
                return error(StatusCode::SERVICE_UNAVAILABLE, "pack_unavailable");
            };
            let mut reply = http::Response::builder()
                .status(StatusCode::OK)
                .header("content-type", content_type)
                .header("content-length", artifact.size())
                .header("cache-control", "no-store")
                .body(super::ReplyBody::Artifact(Box::new(body)))
                .expect("fixed artifact headers");
            let etag = format!("\"{}\"", artifact.sha256().to_hex());
            let Ok(etag) = http::HeaderValue::from_str(&etag) else {
                return error(StatusCode::SERVICE_UNAVAILABLE, "pack_unavailable");
            };
            reply.headers_mut().insert("etag", etag);
            reply
        }
        _ => error(StatusCode::NOT_FOUND, "endpoint_not_found"),
    }
}
fn minecraft_error(code: &'static str) -> Reply {
    match code {
        // Production challenge issuance/completion refill one token per second.
        "minecraft_rate_limited" => rate_limited("minecraft_rate_limited", Duration::from_secs(1)),
        "minecraft_busy" => error(StatusCode::SERVICE_UNAVAILABLE, "minecraft_busy"),
        _ => error(StatusCode::SERVICE_UNAVAILABLE, "minecraft_unavailable"),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Pair {
    code: String,
    device_name: String,
    public_key: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DeviceChallenge {
    device_id: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ManagementSession {
    device_id: String,
    nonce: String,
    signature: String,
}
fn management_error(e: vesta_control::identity::management::ManagementError) -> Reply {
    use vesta_control::identity::management::ManagementError;
    let status = match e {
        ManagementError::PairInvalid | ManagementError::SessionInvalid => StatusCode::UNAUTHORIZED,
        ManagementError::InvalidRequest => StatusCode::BAD_REQUEST,
        ManagementError::Forbidden => StatusCode::FORBIDDEN,
        ManagementError::Saturated | ManagementError::Unavailable => {
            StatusCode::SERVICE_UNAVAILABLE
        }
    };
    error(status, e.code())
}
fn manage_dispatch(
    host: &NetworkHost,
    path: &str,
    public: bool,
    authorization: Option<&str>,
    bytes: &[u8],
) -> Reply {
    let management = host.management();
    if !public {
        return match authorization.map_or(
            Err(vesta_control::identity::management::ManagementError::SessionInvalid),
            |token| {
                management.authorize(token, vesta_control::identity::Capability::StatusView, None)
            },
        ) {
            Ok(_) => error(StatusCode::NOT_IMPLEMENTED, "not_implemented"),
            Err(e) => management_error(e),
        };
    }
    match path {
        "/api/v3/manage/device/enroll" => {
            let Ok(input) = serde_json::from_slice::<DeviceEnrollment>(bytes) else {
                return error(StatusCode::BAD_REQUEST, "invalid_request");
            };
            match management.enroll_device_for(&input.device_name, &input.public_key, authorization)
            {
                Ok(device) => json(serde_json::json!({"device_id":device.device_id})),
                Err(e) => management_error(e),
            }
        }
        "/api/v3/manage/password" => {
            let Some(token) = authorization else {
                return error(StatusCode::UNAUTHORIZED, "device_session_required");
            };
            let Ok(input) = serde_json::from_slice::<Password>(bytes) else {
                return error(StatusCode::BAD_REQUEST, "invalid_request");
            };
            let Some(passwords) = host.passwords() else {
                return error(StatusCode::FORBIDDEN, "pack_access_denied");
            };
            let verification = match passwords.verify(input.password.into_bytes()) {
                Ok(v) => v,
                Err(_) => {
                    return error(StatusCode::SERVICE_UNAVAILABLE, "pack_session_unavailable");
                }
            };
            match host.with_policy(|p| management.grant_password(token, &verification, p)) {
                Ok(Ok(())) => json(serde_json::json!({"granted":true})),
                _ => error(StatusCode::FORBIDDEN, "pack_access_denied"),
            }
        }
        "/api/v3/manage/minecraft/challenge" => {
            let Ok(input) = serde_json::from_slice::<Player>(bytes) else {
                return error(StatusCode::BAD_REQUEST, "invalid_request");
            };
            let (Ok(uuid), Ok(name)) = (
                MinecraftUuid::parse(&input.uuid),
                MinecraftName::parse(&input.name),
            ) else {
                return error(StatusCode::BAD_REQUEST, "invalid_request");
            };
            match host.minecraft().issue_management(uuid, name) {
                Ok(c) => json(
                    serde_json::json!({"challenge": c.challenge_id().as_str(), "server_hash": c.server_hash().as_str(), "expires_in": c.lifetime().as_secs()}),
                ),
                Err(e) => minecraft_error(e.code()),
            }
        }
        "/api/v3/manage/minecraft/complete" => {
            let Ok(input) = serde_json::from_slice::<MinecraftEnrollment>(bytes) else {
                return error(StatusCode::BAD_REQUEST, "invalid_request");
            };
            let Ok(challenge) = ChallengeId::parse(&input.challenge) else {
                return error(StatusCode::UNAUTHORIZED, "management_session_invalid");
            };
            match host.minecraft().complete_management(&challenge) {
                Ok(Some(profile)) => match management.enroll_minecraft_for(
                    profile,
                    &input.device_name,
                    &input.public_key,
                    authorization,
                ) {
                    Ok(enrolled) => json(serde_json::json!({"device_id": enrolled.device_id})),
                    Err(e) => management_error(e),
                },
                Ok(None) => error(StatusCode::UNAUTHORIZED, "management_session_invalid"),
                Err(e) => minecraft_error(e.code()),
            }
        }
        "/api/v3/manage/pair" => {
            let Ok(input) = serde_json::from_slice::<Pair>(bytes) else {
                return error(StatusCode::BAD_REQUEST, "invalid_request");
            };
            let code = zeroize::Zeroizing::new(input.code);
            match management.enroll_for(&code, &input.device_name, &input.public_key, authorization)
            {
                Ok(enrolled) => json(serde_json::to_value(enrolled).expect("enrollment JSON")),
                Err(e) => management_error(e),
            }
        }
        "/api/v3/manage/session/challenge" => {
            let Ok(input) = serde_json::from_slice::<DeviceChallenge>(bytes) else {
                return error(StatusCode::BAD_REQUEST, "invalid_request");
            };
            match management.challenge(&input.device_id) {
                Ok(nonce) => json(
                    serde_json::json!({"server_id": management.server_id(), "nonce": nonce.expose(), "expires_in_seconds": 60, "purpose": vesta_control::identity::management::SESSION_PURPOSE}),
                ),
                Err(e) => management_error(e),
            }
        }
        "/api/v3/manage/session" => {
            let Ok(input) = serde_json::from_slice::<ManagementSession>(bytes) else {
                return error(StatusCode::BAD_REQUEST, "invalid_request");
            };
            match management.session(&input.device_id, &input.nonce, &input.signature) {
                Ok(token) => json(
                    serde_json::json!({"server_id": management.server_id(), "token": token.secret.expose(), "expires_at_ms": token.expires_at_ms}),
                ),
                Err(e) => management_error(e),
            }
        }
        _ => unreachable!("closed public management route set"),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MinecraftEnrollment {
    challenge: String,
    device_name: String,
    public_key: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DeviceEnrollment {
    device_name: String,
    public_key: String,
}

/// Exactly one syntactically valid bearer. No query, cookie or duplicate fallback.
pub(super) fn bearer(headers: &http::HeaderMap) -> Option<&str> {
    if headers.get_all("authorization").iter().count() != 1 {
        return None;
    }
    headers
        .get("authorization")?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
        .filter(|v| {
            v.len() == 43
                && v.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        })
}
