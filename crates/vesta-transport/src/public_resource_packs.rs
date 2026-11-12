//! The only unauthenticated hosting route. Deploy a public TLS reverse proxy for
//! this exact path, not the management API. No filesystem path or token in URLs.
use crate::{ArtifactBody, Reply, ReplyBody, manage::error};
use http::{Method, StatusCode};
use std::sync::Arc;
use tokio::sync::Semaphore;
use vesta_control::{host::NetworkHost, resource_packs::hosting};
pub(crate) fn implemented(path: &str) -> bool {
    path.starts_with("/resource-packs/")
}
pub(crate) fn dispatch(
    host: &NetworkHost,
    method: &Method,
    path: &str,
    downloads: Arc<Semaphore>,
) -> Reply {
    if !matches!(*method, Method::GET | Method::HEAD) {
        return error(StatusCode::METHOD_NOT_ALLOWED, "method_not_allowed");
    }
    let Some(sha) = path
        .strip_prefix("/resource-packs/")
        .and_then(|p| p.strip_suffix(".zip"))
    else {
        return error(StatusCode::NOT_FOUND, "resource_pack_not_found");
    };
    let Ok(permit) = downloads.try_acquire_owned() else {
        return crate::with_retry_after(
            error(StatusCode::SERVICE_UNAVAILABLE, "download_busy"),
            std::time::Duration::from_secs(1),
        );
    };
    let (file, release) = match host.resource_packs().open_public(sha) {
        Ok(v) => v,
        Err(hosting::Error::NotFound) => {
            return error(StatusCode::NOT_FOUND, "resource_pack_not_found");
        }
        Err(_) => return error(StatusCode::SERVICE_UNAVAILABLE, "resource_pack_unavailable"),
    };
    let Ok(expected) = vesta_protocol::pack::catalogue::Sha256::parse(release.artifact.sha256())
    else {
        return error(StatusCode::SERVICE_UNAVAILABLE, "resource_pack_unavailable");
    };
    let body = if *method == Method::HEAD {
        ReplyBody::Bytes(Default::default())
    } else {
        match ArtifactBody::from_identity(
            file,
            release.artifact.size(),
            *expected.as_bytes(),
            permit.into(),
        ) {
            Ok(body) => ReplyBody::Artifact(Box::new(body)),
            Err(_) => return error(StatusCode::SERVICE_UNAVAILABLE, "resource_pack_unavailable"),
        }
    };
    http::Response::builder()
        .status(StatusCode::OK)
        .header("content-type", "application/zip")
        .header("content-length", release.artifact.size())
        .header("cache-control", "public, max-age=31536000, immutable")
        .header("etag", format!("\"{}\"", release.artifact.sha256()))
        .header("x-content-type-options", "nosniff")
        .body(body)
        .expect("fixed headers and validated hash")
}
