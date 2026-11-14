use super::*;
use vesta_control::machine::removed::{self, RemoveRequest};
pub(super) fn dispatch(router: &HostRouter, device: &Device, path: &str, bytes: &[u8]) -> Reply {
    let state = &router.identity.state;
    if !state
        .host_role(device.account_id)
        .is_ok_and(|r| r.is_some())
    {
        return failure(StatusCode::FORBIDDEN, "forbidden");
    }
    let Some(id) = path
        .strip_prefix("/v1/host/servers/")
        .and_then(|s| Uuid::parse_str(s).ok())
    else {
        return failure(StatusCode::NOT_FOUND, "not_found");
    };
    let req: RemoveRequest = match serde_json::from_slice(bytes) {
        Ok(v) => v,
        Err(_) => return failure(StatusCode::BAD_REQUEST, "invalid_request"),
    };
    let _lock = match router.removal_lock.lock() {
        Ok(v) => v,
        Err(_) => return failure(StatusCode::SERVICE_UNAVAILABLE, "unavailable"),
    };
    let (record, complete) = match removed::prepare(state, device.account_id, id, &req) {
        Ok(v) => v,
        Err(e) => return error(e),
    };
    if complete {
        return routes::json(record.reply());
    }
    let Some(jobs) = router.imports() else {
        return failure(StatusCode::SERVICE_UNAVAILABLE, "unavailable");
    };
    let worker = router
        .workers
        .lock()
        .ok()
        .and_then(|w| w.iter().find(|w| w.server_id() == id).cloned());
    if let Some(worker) = worker {
        let subject = match subject(router, worker.as_ref(), device) {
            Ok(v) => v,
            Err(e) => return e,
        };
        let network = match worker.authorize(subject) {
            Ok(v) => v,
            Err(_) => return failure(StatusCode::SERVICE_UNAVAILABLE, "worker_unavailable"),
        };
        let busy=network.database().read(|c| {
            for (table,sql) in [
                ("software_jobs","SELECT EXISTS(SELECT 1 FROM software_jobs WHERE state IN ('queued','staging','activating','recovery_required'))"),
                ("backup_jobs","SELECT EXISTS(SELECT 1 FROM backup_jobs WHERE state IN ('awaiting_confirmation','queued','running'))")
            ] {
                let exists:bool=c.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name=?)",[table],|r| r.get(0))?;
                if exists && c.query_row(sql,[],|r|r.get::<_,bool>(0))? { return Ok(true); }
            }
            Ok(false)
        });
        if !matches!(busy, Ok(false)) {
            return failure(StatusCode::CONFLICT, "server_busy");
        }
        let runtime = network.runtime();
        let snapshot = match runtime.game_snapshot() {
            Ok(v) => v,
            Err(_) => return failure(StatusCode::SERVICE_UNAVAILABLE, "worker_unavailable"),
        };
        if let Some(generation) = snapshot
            .generation
            .filter(|_| snapshot.phase != vesta_control::operations::ProcessPhase::Stopped)
        {
            let actor = vesta_control::identity::Subject {
                principal: vesta_control::identity::PrincipalId::parse(
                    &device.account_id.to_string(),
                )
                .unwrap(),
                device: vesta_control::identity::DeviceId::parse(&device.device_id.to_string())
                    .unwrap(),
            };
            if runtime
                .submit_game(
                    &actor,
                    vesta_control::operations::GameCommand::Stop {
                        expected_generation: generation,
                    },
                    &format!("remove-{}", req.idempotency_key),
                )
                .is_err()
            {
                return failure(StatusCode::CONFLICT, "server_busy");
            }
            let until = std::time::Instant::now() + Duration::from_secs(60);
            while !runtime
                .game_snapshot()
                .is_ok_and(|s| s.phase == vesta_control::operations::ProcessPhase::Stopped)
            {
                if std::time::Instant::now() >= until {
                    return failure(StatusCode::CONFLICT, "server_busy");
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        }
        let generation = match state.snapshot() {
            Ok(s) => s.auth_generation.saturating_add(1),
            Err(_) => return failure(StatusCode::SERVICE_UNAVAILABLE, "unavailable"),
        };
        if worker
            .sync_policy(WorkerPolicy {
                host_id: state.snapshot().unwrap().host_id,
                server_id: id,
                generation,
                grants: Vec::new(),
            })
            .is_err()
        {
            return failure(StatusCode::SERVICE_UNAVAILABLE, "worker_unavailable");
        }
    }
    let owned = router.owned.lock().ok().and_then(|mut w| {
        w.iter()
            .position(|w| w.handle().server_id() == id)
            .map(|i| w.remove(i))
    });
    if let Some(worker) = owned {
        match worker.shutdown(Duration::from_secs(60)) {
            Ok(vesta_control::worker::WorkerShutdown::Complete) => (),
            Ok(vesta_control::worker::WorkerShutdown::TimedOut(worker)) => {
                router.owned.lock().unwrap().push(*worker);
                return failure(StatusCode::CONFLICT, "server_busy");
            }
            Err(_) => return failure(StatusCode::SERVICE_UNAVAILABLE, "worker_unavailable"),
        }
    }
    if let Err(e) = removed::finish(state, jobs.servers_root(), &record, &req) {
        return error(e);
    }
    router
        .workers
        .lock()
        .unwrap()
        .retain(|w| w.server_id() != id);
    routes::json(record.reply())
}
fn error(e: JobError) -> Reply {
    let status = match e.0 {
        "forbidden" => StatusCode::FORBIDDEN,
        "not_found" => StatusCode::NOT_FOUND,
        "confirm_name_mismatch" => StatusCode::UNPROCESSABLE_ENTITY,
        "server_busy" | "idempotency_conflict" => StatusCode::CONFLICT,
        _ => StatusCode::SERVICE_UNAVAILABLE,
    };
    failure(status, e.0)
}
