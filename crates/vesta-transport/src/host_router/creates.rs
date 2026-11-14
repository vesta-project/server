//! Host-owner creation catalogue, defaults and `new` server jobs
//! (`protocol/host-v1/create.md`); invoked only after current host
//! authorization. Long work (resolving, downloads, install) runs on a host
//! background thread and is polled through the durable job.
use super::*;
use vesta_control::machine::{
    creates::{self, CreateInstaller},
    imports::jobs::{self, ImportJobs},
};

pub(super) fn dispatch(
    router: &HostRouter,
    device: &Device,
    method: &Method,
    path: &str,
    query: Option<&str>,
    headers: &http::HeaderMap,
    bytes: &[u8],
) -> Reply {
    // Modes that are not built yet answer 501 so the launcher can hide them.
    if path == "/v1/host/creates/restore-sources" || path == "/v1/host/creates/uploads" {
        return failure(StatusCode::NOT_IMPLEMENTED, "not_supported_yet");
    }
    if path == "/v1/host/creates/plan"
        || path == "/v1/host/creates/apply"
        || path.starts_with("/v1/host/creates/jobs/")
    {
        return job_routes(router, device, method, path, headers, bytes);
    }
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

/// Everything a running create job needs, cloned into its background thread.
#[derive(Clone)]
struct Service {
    jobs: ImportJobs,
    installer: Arc<dyn CreateInstaller>,
    catalogue: Arc<dyn creates::Catalogue>,
}

fn job_routes(
    router: &HostRouter,
    device: &Device,
    method: &Method,
    path: &str,
    headers: &http::HeaderMap,
    bytes: &[u8],
) -> Reply {
    let binding = OwnerBinding {
        host_id: match router.identity.state.snapshot() {
            Ok(s) => s.host_id.to_string(),
            Err(_) => return failure(StatusCode::SERVICE_UNAVAILABLE, "unavailable"),
        },
        account_id: device.account_id.to_string(),
        device_id: device.device_id.to_string(),
    };
    macro_rules! tryjob {
        ($v:expr) => {
            match $v {
                Ok(v) => v,
                Err(e) => return job_failure(e),
            }
        };
    }
    if path == "/v1/host/creates/plan" {
        if method != Method::POST {
            return failure(StatusCode::NOT_FOUND, "not_found");
        }
        // The kind decides the shape; only `new` exists so far.
        let value: serde_json::Value = match parse(bytes) {
            Ok(v) => v,
            Err(e) => return e,
        };
        match value.get("kind").and_then(serde_json::Value::as_str) {
            Some("new") => {}
            Some("modpack" | "restore") => {
                return failure(StatusCode::NOT_IMPLEMENTED, "not_supported_yet");
            }
            _ => return failure(StatusCode::BAD_REQUEST, "invalid_request"),
        }
        let Ok(request) = serde_json::from_value::<jobs::NewServerRequest>(value) else {
            return failure(StatusCode::BAD_REQUEST, "invalid_request");
        };
        let Some(service) = service(router) else {
            return failure(StatusCode::SERVICE_UNAVAILABLE, "creates_unavailable");
        };
        let accepted = tryjob!(service.jobs.begin_create(&binding, &request));
        let (worker, owner, id) = (service, binding, accepted.job_id.clone());
        background(move || {
            let _ = worker.jobs.run_create_plan(
                &owner,
                &id,
                worker.installer.as_ref(),
                worker.catalogue.as_ref(),
            );
        });
        return accepted_reply(serde_json::to_value(accepted).expect("wire"));
    }
    let Some(service) = service(router) else {
        return failure(StatusCode::SERVICE_UNAVAILABLE, "creates_unavailable");
    };
    let jobs = &service.jobs;
    if path == "/v1/host/creates/apply" {
        if method != Method::POST {
            return failure(StatusCode::NOT_FOUND, "not_found");
        }
        let Some(key) = headers.get("idempotency-key").and_then(|v| v.to_str().ok()) else {
            return failure(StatusCode::BAD_REQUEST, "idempotency_key_required");
        };
        let request: jobs::CreateApplyRequest = match parse(bytes) {
            Ok(v) => v,
            Err(e) => return e,
        };
        let accepted = tryjob!(jobs.create_apply(&binding, &request, key));
        spawn_apply(&service, &binding, &accepted.job_id);
        return accepted_reply(serde_json::to_value(accepted).expect("wire"));
    }
    let rest = path.trim_start_matches("/v1/host/creates/jobs/");
    let (id, action) = rest.split_once('/').unwrap_or((rest, ""));
    if Uuid::parse_str(id).is_err() {
        return failure(StatusCode::NOT_FOUND, "create_job_not_found");
    }
    if !matches!(
        (method.as_str(), action),
        ("GET", "") | ("POST", "cancel" | "resume" | "start")
    ) {
        return failure(StatusCode::NOT_FOUND, "not_found");
    }
    if method == Method::POST && !bytes.is_empty() && bytes != b"{}" {
        return failure(StatusCode::BAD_REQUEST, "invalid_request");
    }
    match action {
        "" => {
            let mut job = tryjob!(jobs.create_status(&binding, id));
            if job.state == "starting"
                && let Some(handle) = job.server_id.and_then(|s| router.owned_handle(s))
            {
                job = tryjob!(jobs.create_readiness(&binding, id, &handle));
                router.refresh_server_states();
            }
            routes::json(job_json(&job))
        }
        "cancel" => {
            let response = tryjob!(jobs.create_cancel(&binding, id));
            routes::json(serde_json::to_value(response).expect("wire"))
        }
        "resume" => {
            let job = tryjob!(jobs.create_resume(&binding, id));
            if job.state == "applying" {
                spawn_apply(&service, &binding, id);
            }
            accepted_reply(job_json(&job))
        }
        _ => {
            let job = tryjob!(jobs.create_status(&binding, id));
            let Some(server) = job.server_id else {
                return job_failure(JobError::CONFLICT);
            };
            if !matches!(job.state.as_str(), "ready" | "starting" | "online") {
                return job_failure(JobError::CONFLICT);
            }
            let handle = tryjob!(router.import_worker(server));
            let job = tryjob!(jobs.create_start(&binding, id, &handle));
            router.refresh_server_states();
            accepted_reply(job_json(&job))
        }
    }
}

fn service(router: &HostRouter) -> Option<Service> {
    Some(Service {
        jobs: router.imports()?.clone(),
        installer: router.creator.clone()?,
        catalogue: router.catalogue.clone()?,
    })
}
fn spawn_apply(service: &Service, binding: &OwnerBinding, id: &str) {
    let (worker, owner, id) = (service.clone(), binding.clone(), id.to_owned());
    // A second runner for the same job is refused by `run_create_apply` itself.
    background(move || {
        let _ = worker
            .jobs
            .run_create_apply(&owner, &id, worker.installer.as_ref());
    });
}
fn background(work: impl FnOnce() + Send + 'static) {
    // If no thread can be started the job stays pollable; a host restart
    // marks it `interrupted` and the owner resumes it.
    let _ = std::thread::Builder::new()
        .name("vesta-create-job".into())
        .spawn(work);
}
fn accepted_reply(value: serde_json::Value) -> Reply {
    let mut reply = routes::json(value);
    *reply.status_mut() = StatusCode::ACCEPTED;
    reply
}
/// Wire form: RFC 3339 `review.expiresAt`, everything else as the job type.
fn job_json(job: &jobs::CreateJob) -> serde_json::Value {
    let mut value = serde_json::to_value(job).expect("wire");
    if let Some(ms) = value
        .pointer("/review/expiresAt")
        .and_then(serde_json::Value::as_i64)
    {
        value["review"]["expiresAt"] = json!(
            chrono::DateTime::from_timestamp_millis(ms)
                .map(|d| d.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
        );
    }
    value
}
fn job_failure(error: JobError) -> Reply {
    let status = match error.code() {
        "forbidden" => StatusCode::FORBIDDEN,
        "create_job_not_found" => StatusCode::NOT_FOUND,
        "invalid_request" | "eula_required" | "warnings_unacknowledged" => StatusCode::BAD_REQUEST,
        "unavailable" | "create_jobs_saturated" | "worker_unavailable" => {
            StatusCode::SERVICE_UNAVAILABLE
        }
        _ => StatusCode::CONFLICT,
    };
    failure(status, error.code())
}
