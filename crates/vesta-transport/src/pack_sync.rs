//! Fresh signed latest-release statements and revision-pinned downloads.
use crate::manage::{error, ok};
use crate::{Reply, routes::Requester, tls::Material};
use http::{Method, StatusCode};
use std::time::Instant;
use vesta_control::{host::NetworkHost, packs::ArtifactFormat};

pub(crate) fn implemented(path: &str) -> bool {
    matches!(
        path,
        "/api/v3/manage/pack/sync" | "/api/v3/manage/pack/artifact"
    )
}

pub(crate) fn dispatch(
    host: &NetworkHost,
    method: &Method,
    path: &str,
    query: Option<&str>,
    token: Option<&str>,
    requester: Requester,
    tls: &Material,
) -> Reply {
    if *method != Method::GET {
        return error(StatusCode::METHOD_NOT_ALLOWED, "method_not_allowed");
    }
    // Includes automatic/password pack roles; revocation is checked each time.
    let Ok(Ok((reference, _))) = host.with_policy(|p| {
        host.sessions()
            .authenticate_reference(token.unwrap_or(""), Instant::now(), p)
    }) else {
        return error(StatusCode::FORBIDDEN, "pack_access_denied");
    };
    let release = match host.releases().offered() {
        Ok(Some(r)) => r,
        Ok(None) => return error(StatusCode::NOT_FOUND, "pack_not_published"),
        Err(_) => return error(StatusCode::SERVICE_UNAVAILABLE, "pack_unavailable"),
    };
    let Some((first, second)) = query.and_then(|q| q.split_once('&')) else {
        return error(StatusCode::BAD_REQUEST, "invalid_request");
    };
    let artifact = release.artifact();
    if path.ends_with("/sync") {
        let Some(installed) = first.strip_prefix("installed=").and_then(decimal) else {
            return error(StatusCode::BAD_REQUEST, "invalid_request");
        };
        let Some(nonce) = second
            .strip_prefix("nonce=")
            .filter(|v| v.len() == 32 && v.bytes().all(|b| b.is_ascii_hexdigit()))
        else {
            return error(StatusCode::BAD_REQUEST, "invalid_request");
        };
        if installed > release.revision() {
            return error(StatusCode::CONFLICT, "pack_rollback_rejected");
        }
        let format = match artifact.format() {
            ArtifactFormat::Vesta => "vestapack",
            ArtifactFormat::Mrpack => "mrpack",
            ArtifactFormat::Zip => "zip",
            ArtifactFormat::Catalogue => "catalogue",
        };
        let mut value = serde_json::json!({"server_id":host.management().server_id(),"revision":release.revision().to_string(),"minimum_revision":release.minimum_revision().to_string(),"sha256":artifact.sha256().to_hex(),"size":artifact.size().to_string(),"format":format,"installed_revision":installed.to_string(),"status":release.status_for(installed).wire(),"nonce":nonce});
        return match tls.sign_pack(&mut value) {
            Ok(v) => ok(v),
            Err(_) => error(
                StatusCode::SERVICE_UNAVAILABLE,
                "pack_signature_unavailable",
            ),
        };
    }
    if first != format!("revision={}", release.revision())
        || second != format!("sha256={}", artifact.sha256().to_hex())
    {
        return error(StatusCode::PRECONDITION_FAILED, "pack_release_conflict");
    }
    let Some(permit) = requester.downloads.acquire(reference, requester.peer) else {
        return error(StatusCode::SERVICE_UNAVAILABLE, "download_busy");
    };
    let Ok(file) = host.releases().artifacts().open_pinned(artifact) else {
        return error(StatusCode::SERVICE_UNAVAILABLE, "pack_unavailable");
    };
    let Ok(body) = crate::ArtifactBody::new(file, artifact, permit) else {
        return error(StatusCode::SERVICE_UNAVAILABLE, "pack_unavailable");
    };
    http::Response::builder()
        .status(StatusCode::OK)
        .header("content-type", "application/octet-stream")
        .header("content-length", artifact.size())
        .header("cache-control", "no-store")
        .body(crate::ReplyBody::Artifact(Box::new(body)))
        .expect("fixed headers")
}
fn decimal(value: &str) -> Option<u64> {
    if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let n = value.parse::<u64>().ok()?;
    (n.to_string() == value).then_some(n)
}
