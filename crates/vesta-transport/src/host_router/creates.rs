//! Host-owner creation metadata; invoked only after current host authorization.
use super::*;
use vesta_control::machine::creates;

pub(super) fn dispatch(
    router: &HostRouter,
    method: &Method,
    path: &str,
    query: Option<&str>,
) -> Reply {
    if method != Method::GET {
        return failure(StatusCode::NOT_FOUND, "not_found");
    }
    let Some(catalogue) = router.catalogue.as_deref() else {
        return failure(StatusCode::SERVICE_UNAVAILABLE, "creates_unavailable");
    };
    let result = match path {
        "/v1/host/catalogue/minecraft" => creates::minecraft(catalogue),
        "/v1/host/catalogue/software" => {
            let pairs: Vec<_> =
                url::form_urlencoded::parse(query.unwrap_or("").as_bytes()).collect();
            if pairs.len() != 1 || pairs[0].0 != "minecraftVersion" {
                return failure(StatusCode::BAD_REQUEST, "invalid_request");
            }
            creates::software(catalogue, &pairs[0].1)
        }
        "/v1/host/creates/defaults" => {
            let Some(jobs) = router.imports() else {
                return failure(StatusCode::SERVICE_UNAVAILABLE, "creates_unavailable");
            };
            creates::defaults(&router.identity.state, jobs, catalogue)
        }
        _ => return failure(StatusCode::NOT_FOUND, "not_found"),
    };
    match result {
        Ok(value) => routes::json(value),
        Err(e) => failure(
            if e == JobError::INVALID {
                StatusCode::BAD_REQUEST
            } else {
                StatusCode::SERVICE_UNAVAILABLE
            },
            e.code(),
        ),
    }
}
