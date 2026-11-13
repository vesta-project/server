//! Owner-authenticated folders, inspection and import jobs.
use super::*;
use vesta_control::machine::{
    imports::{self as domain, jobs},
    resources,
};
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Resources {
    game_port: Option<u16>,
    memory_mi_b: Option<u64>,
}
pub(super) fn dispatch(
    router: &HostRouter,
    device: &Device,
    method: &Method,
    path: &str,
    headers: &http::HeaderMap,
    bytes: &[u8],
) -> Reply {
    let binding = OwnerBinding {
        host_id: router
            .identity
            .state
            .snapshot()
            .unwrap()
            .host_id
            .to_string(),
        account_id: device.account_id.to_string(),
        device_id: device.device_id.to_string(),
    };
    macro_rules! input {
        () => {
            match parse(bytes) {
                Ok(v) => v,
                Err(e) => return e,
            }
        };
    }
    macro_rules! result {
        ($v:expr) => {
            match $v {
                Ok(v) => routes::json(serde_json::to_value(v).unwrap()),
                Err(e) => failure(StatusCode::BAD_REQUEST, e.code()),
            }
        };
    }
    match (method.as_str(), path) {
        ("GET", "/v1/host/folders/roots") => result!(router.folders.roots()),
        ("POST", "/v1/host/folders/resolve") => {
            let r: Resolve = input!();
            result!(router.folders.resolve(
                &binding,
                &domain::ResolveRequest {
                    root_id: r.root_id,
                    relative_path: r.relative_path
                }
            ))
        }
        ("POST", "/v1/host/folders/check") => {
            let r: Check = input!();
            result!(router.folders.check(&binding, &r.path))
        }
        ("POST", "/v1/host/folders/list") => {
            let r: List = input!();
            result!(router.folders.list(
                &binding,
                &domain::ListRequest {
                    folder_token: r.folder_token,
                    cursor: r.cursor
                }
            ))
        }
        ("GET", "/v1/host/imports/candidates") => result!(domain::amp::candidates(
            &router.folders,
            &binding,
            &domain::CandidateScan::default()
        )),
        ("POST", "/v1/host/imports/inspect") => {
            let r: Inspect = input!();
            result!(domain::inspection::inspect(
                &router.folders,
                &binding,
                &domain::InspectRequest {
                    folder_token: r.folder_token,
                    original_path: None,
                    destination: None,
                    game_port: r.game_port
                }
            ))
        }
        ("POST", "/v1/host/imports/resources") => {
            let request: Resources = input!();
            let mut reservations = resources::Reservations::default();
            if let Ok(entries) = router.identity.state.visible_servers(device.account_id) {
                for (e, _) in entries {
                    reservations.ports.insert(e.game_port);
                }
            }
            // No heap reservation catalogue exists yet: refuse to guess remaining RAM.
            let memory = if reservations.ports.is_empty() {
                resources::plan_host_memory(&reservations, request.memory_mi_b)
            } else {
                resources::plan_memory(None, &reservations, request.memory_mi_b)
            };
            routes::json(
                json!({"memory":memory,"port":resources::plan_port(&reservations,request.game_port)}),
            )
        }
        _ if path == "/v1/host/imports/plan"
            || path == "/v1/host/imports/apply"
            || path.starts_with("/v1/host/imports/jobs/") =>
        {
            match router.imports.clone() {
                Some(service) => {
                    job_routes(router, &service, &binding, method, path, headers, bytes)
                }
                None => failure(StatusCode::SERVICE_UNAVAILABLE, "imports_unavailable"),
            }
        }
        _ => failure(StatusCode::NOT_FOUND, "not_found"),
    }
}

/// Plan, apply, status, cancel, resume and start. Long work (inspection,
/// hashing, copying, configuring) runs on a background thread and is polled
/// through the durable job; every HTTP call returns promptly.
fn job_routes(
    router: &HostRouter,
    service: &Arc<ImportService>,
    binding: &OwnerBinding,
    method: &Method,
    path: &str,
    headers: &http::HeaderMap,
    bytes: &[u8],
) -> Reply {
    let jobs = &service.jobs;
    macro_rules! tryjob {
        ($v:expr) => {
            match $v {
                Ok(v) => v,
                Err(e) => return job_failure(e),
            }
        };
    }
    match (method.as_str(), path) {
        ("POST", "/v1/host/imports/plan") => {
            let request: jobs::PlanRequest = match parse(bytes) {
                Ok(v) => v,
                Err(e) => return e,
            };
            let accepted = tryjob!(jobs.begin_plan(binding, request));
            let (worker, owner, id) = (jobs.clone(), binding.clone(), accepted.job_id.clone());
            background(move || {
                let _ = worker.run_plan(&owner, &id);
            });
            accepted_reply(serde_json::to_value(accepted).expect("wire"))
        }
        ("POST", "/v1/host/imports/apply") => {
            let Some(key) = headers.get("idempotency-key").and_then(|v| v.to_str().ok()) else {
                return failure(StatusCode::BAD_REQUEST, "idempotency_key_required");
            };
            let request: jobs::ApplyRequest = match parse(bytes) {
                Ok(v) => v,
                Err(e) => return e,
            };
            let accepted = tryjob!(jobs.apply(binding, &request, key));
            spawn_apply(jobs, binding, &accepted.job_id);
            accepted_reply(serde_json::to_value(accepted).expect("wire"))
        }
        _ => {
            let rest = path.trim_start_matches("/v1/host/imports/jobs/");
            let (id, action) = rest.split_once('/').unwrap_or((rest, ""));
            if Uuid::parse_str(id).is_err() {
                return failure(StatusCode::NOT_FOUND, "import_job_not_found");
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
                    let mut job = tryjob!(jobs.status(binding, id));
                    if job.state == "starting"
                        && let Some(handle) = job.server_id.and_then(|s| router.owned_handle(s))
                    {
                        job = tryjob!(jobs.readiness(binding, id, &handle));
                        router.refresh_server_states();
                    }
                    routes::json(job_json(&job))
                }
                "cancel" => {
                    let response = tryjob!(jobs.cancel(binding, id));
                    routes::json(serde_json::to_value(response).expect("wire"))
                }
                "resume" => {
                    let job = tryjob!(jobs.resume(binding, id));
                    if job.state == "applying" {
                        spawn_apply(jobs, binding, id);
                    }
                    accepted_reply(job_json(&job))
                }
                _ => {
                    let job = tryjob!(jobs.status(binding, id));
                    let Some(server) = job.server_id else {
                        return job_failure(JobError::CONFLICT);
                    };
                    if !matches!(job.state.as_str(), "ready" | "starting" | "online") {
                        return job_failure(JobError::CONFLICT);
                    }
                    let handle = tryjob!(router.import_worker(server));
                    let job = tryjob!(jobs.start(binding, id, &handle));
                    router.refresh_server_states();
                    accepted_reply(job_json(&job))
                }
            }
        }
    }
}
fn spawn_apply(jobs: &ImportJobs, binding: &OwnerBinding, id: &str) {
    let (worker, owner, id) = (jobs.clone(), binding.clone(), id.to_owned());
    // A second runner for the same job is refused by `run_apply` itself.
    background(move || {
        let _ = worker.run_apply(&owner, &id);
    });
}
fn background(work: impl FnOnce() + Send + 'static) {
    // If no thread can be started the job stays pollable; a host restart
    // marks it `interrupted` and the owner resumes it.
    let _ = std::thread::Builder::new()
        .name("vesta-import-job".into())
        .spawn(work);
}
fn accepted_reply(value: serde_json::Value) -> Reply {
    let mut reply = routes::json(value);
    *reply.status_mut() = StatusCode::ACCEPTED;
    reply
}
/// Wire form: RFC 3339 `review.expiresAt`, everything else as the job type.
fn job_json(job: &jobs::ImportJob) -> serde_json::Value {
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
        "import_job_not_found" => StatusCode::NOT_FOUND,
        "invalid_request"
        | "eula_required"
        | "warnings_unacknowledged"
        | "source_stop_confirmation_required" => StatusCode::BAD_REQUEST,
        "unavailable" | "import_jobs_saturated" | "worker_unavailable" => {
            StatusCode::SERVICE_UNAVAILABLE
        }
        _ => StatusCode::CONFLICT,
    };
    failure(status, error.code())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Resolve {
    root_id: String,
    relative_path: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Check {
    path: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct List {
    folder_token: String,
    cursor: Option<String>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Inspect {
    folder_token: String,
    game_port: Option<u16>,
}
