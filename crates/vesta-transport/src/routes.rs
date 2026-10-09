use super::{Reply, Shared, error, response};
use http::{Method, Request, StatusCode};
use http_body_util::BodyExt;
use hyper::body::Incoming;
use serde::Deserialize;
use std::{sync::Arc, time::Instant};
use vesta_control::{
    host::NetworkHost,
    identity::{
        PasswordVerificationStatus,
        minecraft::{ChallengeId, MinecraftName, MinecraftUuid},
    },
    packs::{ArtifactFormat, MAX_ARTIFACT_BYTES},
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

pub(super) async fn route(request: Request<Incoming>, state: Arc<Shared>) -> Reply {
    let path = request.uri().path().to_owned();
    let method = request.method().clone();
    let recognized = matches!(
        (method.as_str(), path.as_str()),
        (
            "POST",
            "/api/v2/discovery"
                | "/api/v2/auth/password"
                | "/api/v2/auth/minecraft/challenge"
                | "/api/v2/auth/minecraft/complete"
                | "/api/v2/pack/ticket"
        ) | ("GET", "/api/v2/pack/status" | "/api/v2/pack/artifact")
    );
    if !recognized {
        return error(StatusCode::NOT_FOUND, "endpoint_not_found");
    }
    let query = request.uri().query().map(str::to_owned);
    if path != "/api/v2/pack/status" && query.is_some() {
        return error(StatusCode::BAD_REQUEST, "invalid_request");
    }
    // Exactly one observed Authorization field; never accept query/cookie secrets.
    let authorization = if request.headers().get_all("authorization").iter().count() == 1 {
        request
            .headers()
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .filter(|v| {
                v.len() == 43
                    && v.bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
            })
            .map(|secret| zeroize::Zeroizing::new(secret.to_owned()))
    } else {
        None
    };
    if matches!(
        path.as_str(),
        "/api/v2/pack/ticket" | "/api/v2/pack/artifact"
    ) && authorization.is_none()
    {
        return error(StatusCode::UNAUTHORIZED, "pack_session_invalid");
    }
    if method == Method::POST
        && path != "/api/v2/pack/ticket"
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
    if (method == Method::GET || path == "/api/v2/pack/ticket") && !bytes.is_empty() {
        return error(StatusCode::BAD_REQUEST, "invalid_request");
    }
    state.jobs.reap(false).await;
    let host = state.host.clone();
    let downloads = state.downloads.clone();
    state
        .jobs
        .execute(move || {
            dispatch(
                &host,
                &path,
                query.as_deref(),
                authorization.as_ref().map(|secret| secret.as_str()),
                &bytes,
                downloads,
            )
        })
        .await
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
    downloads: Arc<tokio::sync::Semaphore>,
) -> Reply {
    match path {
        "/api/v2/discovery" => {
            let manifest = discovery::decode_request(bytes)
                .and_then(|request| host.discover(&request))
                .and_then(|manifest| discovery::encode_manifest(&manifest));
            match manifest {
                Ok(bytes) => response(StatusCode::OK, bytes, "application/json"),
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
                    return error(StatusCode::TOO_MANY_REQUESTS, "password_rate_limited");
                }
                PasswordVerificationStatus::Busy => {
                    return error(StatusCode::SERVICE_UNAVAILABLE, "password_busy");
                }
                _ => {}
            }
            match host.with_policy(|policy| policy.grant_password(&verification)) {
                Ok(Some(grant)) => issue_session(host, grant),
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
                Ok(Some(profile)) => match host.with_policy(|p| p.grant_minecraft(&profile)) {
                    Ok(Some(grant)) => issue_session(host, grant),
                    _ => error(StatusCode::FORBIDDEN, "pack_access_denied"),
                },
                Ok(None) => error(StatusCode::FORBIDDEN, "pack_access_denied"),
                Err(e) => minecraft_error(e.code()),
            }
        }
        "/api/v2/pack/ticket" => {
            let secret = authorization.unwrap_or("");
            match host.with_policy(|p| host.sessions().issue_ticket(secret, Instant::now(), p)) {
                Ok(Ok(ticket)) => json(serde_json::json!({"ticket": ticket.expose()})),
                Ok(Err(vesta_control::pack_access::PackAccessError::Invalid)) => {
                    error(StatusCode::UNAUTHORIZED, "pack_session_invalid")
                }
                _ => error(StatusCode::SERVICE_UNAVAILABLE, "pack_session_unavailable"),
            }
        }
        "/api/v2/pack/artifact" => {
            let secret = authorization.unwrap_or("");
            if !matches!(
                host.with_policy(|p| host.sessions().authenticate(secret, Instant::now(), p)),
                Ok(Ok(_))
            ) {
                return error(StatusCode::UNAUTHORIZED, "pack_session_invalid");
            }
            let release = match host.releases().current() {
                Ok(Some(r)) => r,
                Ok(None) => return error(StatusCode::NOT_FOUND, "pack_not_published"),
                Err(_) => return error(StatusCode::SERVICE_UNAVAILABLE, "pack_unavailable"),
            };
            let Ok(permit) = downloads.try_acquire_owned() else {
                return error(StatusCode::SERVICE_UNAVAILABLE, "download_busy");
            };
            let artifact = release.artifact();
            let Ok(loaded) = host
                .releases()
                .artifacts()
                .load(artifact, MAX_ARTIFACT_BYTES)
            else {
                return error(StatusCode::SERVICE_UNAVAILABLE, "pack_unavailable");
            };
            let content_type = match artifact.format() {
                ArtifactFormat::Vesta => "application/vnd.vesta.pack",
                ArtifactFormat::Zip | ArtifactFormat::Mrpack => "application/zip",
            };
            let bytes = hyper::body::Bytes::from_owner(super::DownloadOwner {
                data: super::ArtifactOwner(loaded),
                _permit: permit,
            });
            let mut reply = response(StatusCode::OK, bytes, content_type);
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
fn issue_session(host: &NetworkHost, grant: vesta_control::pack_access::PackGrant) -> Reply {
    let now = Instant::now();
    match host.sessions().issue(grant, now) {
        Ok((secret, session)) => json(
            serde_json::json!({"session": secret.expose(), "expires_in": session.expires_at().saturating_duration_since(now).as_secs()}),
        ),
        Err(_) => error(StatusCode::SERVICE_UNAVAILABLE, "pack_session_unavailable"),
    }
}
fn minecraft_error(code: &'static str) -> Reply {
    match code {
        "minecraft_rate_limited" => error(StatusCode::TOO_MANY_REQUESTS, "minecraft_rate_limited"),
        "minecraft_busy" => error(StatusCode::SERVICE_UNAVAILABLE, "minecraft_busy"),
        _ => error(StatusCode::SERVICE_UNAVAILABLE, "minecraft_unavailable"),
    }
}
