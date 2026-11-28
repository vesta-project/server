use super::*;
use std::sync::Weak;
use vesta_control::machine::removed::{self, RemoveRequest};

pub(super) fn start_janitor(app: Weak<Application>) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(5));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tick.tick().await;
            let Some(router) = app.upgrade().map(|app| app.router.clone()) else {
                return;
            };
            if router.is_stopping() {
                return;
            }
            let _ = tokio::task::spawn_blocking(move || {
                enablement::reap(&router);
                if let Some(jobs) = router.imports() {
                    let state = &router.identity.state;
                    if removed::purge_expired(state, jobs.servers_root(), state.now_ms())
                        .is_ok_and(|purged| purged > 0)
                    {
                        router.storage.refresh();
                    }
                }
            })
            .await;
        }
    });
}

pub(super) fn removed_routes(
    router: &HostRouter,
    device: &Device,
    method: &Method,
    path: &str,
    query: Option<&str>,
    headers: &http::HeaderMap,
    bytes: &[u8],
) -> Reply {
    let state = &router.identity.state;
    match state.host_role(device.account_id) {
        Ok(Some(_)) => (),
        Ok(None) => return failure(StatusCode::FORBIDDEN, "forbidden"),
        Err(_) => return failure(StatusCode::SERVICE_UNAVAILABLE, "host_unavailable"),
    }
    let Some(jobs) = router.imports() else {
        return failure(StatusCode::SERVICE_UNAVAILABLE, "unavailable");
    };
    let servers = jobs.servers_root();
    let reply = |value| routes::json(value);
    if *method == Method::GET && path == "/v1/host/servers/removed" {
        let cursor = match query {
            None => None,
            Some(raw) => match raw.strip_prefix("cursor=").filter(|s| !s.is_empty()) {
                Some(cursor) => Some(cursor),
                None => return failure(StatusCode::BAD_REQUEST, "invalid_request"),
            },
        };
        return match removed::list_page(state, servers, device.account_id, cursor) {
            Ok(mut value) => {
                if let Ok(facts) = jobs.server_facts()
                    && let Some(entries) = value["servers"].as_array_mut()
                {
                    for entry in entries {
                        let id = entry["serverId"]
                            .as_str()
                            .and_then(|id| Uuid::parse_str(id).ok());
                        if let Some(bytes) = id.and_then(|id| router.storage.removed_size(id)) {
                            entry["bytes"] = json!(bytes);
                        }
                        if let Some(fact) = id.and_then(|id| facts.get(&id)) {
                            entry["minecraftVersion"] = json!(fact.minecraft_version);
                            entry["software"] = json!(
                                fact.software
                                    .as_ref()
                                    .map(|(kind, version)| json!({"kind":kind,"version":version}))
                            );
                        }
                    }
                }
                reply(value)
            }
            Err(e) => error(e),
        };
    }
    let Some(suffix) = path.strip_prefix("/v1/host/servers/removed/") else {
        return failure(StatusCode::NOT_FOUND, "not_found");
    };
    let (id, action) = suffix.split_once('/').unwrap_or((suffix, ""));
    let Ok(id) = Uuid::parse_str(id) else {
        return failure(StatusCode::NOT_FOUND, "not_found");
    };
    let headers = headers.get_all("idempotency-key");
    let mut values = headers.iter();
    let key = values
        .next()
        .and_then(|v| v.to_str().ok())
        .and_then(|v| Uuid::parse_str(v).ok());
    let Some(key) = key.filter(|_| values.next().is_none()) else {
        return failure(StatusCode::BAD_REQUEST, "idempotency_key_required");
    };
    let _guard = match router.removal_lock.lock() {
        Ok(guard) => guard,
        Err(_) => return failure(StatusCode::SERVICE_UNAVAILABLE, "unavailable"),
    };
    match (method, action) {
        (&Method::POST, "restore") => {
            let request = match parse(bytes) {
                Ok(request) => request,
                Err(reply) => return reply,
            };
            removed::restore_remote(state, servers, device.account_id, id, key, &request)
                .map(reply)
                .unwrap_or_else(error)
        }
        (&Method::DELETE, "") => {
            let request = match parse(bytes) {
                Ok(request) => request,
                Err(reply) => return reply,
            };
            let measured = router
                .storage
                .removed_size(id)
                .or_else(|| router.storage.active_size(id));
            match removed::queue_purge(
                state,
                servers,
                device.account_id,
                id,
                key,
                &request,
                measured,
            ) {
                Ok(value) => {
                    router.storage.refresh();
                    accepted(value)
                }
                Err(e) => error(e),
            }
        }
        _ => failure(StatusCode::NOT_FOUND, "not_found"),
    }
}

fn accepted(value: serde_json::Value) -> Reply {
    let mut reply = routes::json(value);
    *reply.status_mut() = StatusCode::ACCEPTED;
    reply
}

fn remove_reply(record: &removed::Removed) -> Reply {
    if record.permanently {
        accepted(record.purge_reply())
    } else {
        routes::json(record.reply())
    }
}
pub(super) fn dispatch(router: &HostRouter, device: &Device, path: &str, bytes: &[u8]) -> Reply {
    let state = &router.identity.state;
    if !state
        .has_host_capability(
            Actor::Account(device.account_id),
            vesta_control::identity::Capability::HostServersRemove,
        )
        .is_ok_and(|allowed| allowed)
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
    if state.server(id).is_ok_and(|entry| {
        entry.is_some_and(|entry| {
            entry.state == vesta_control::machine::registry::ServerState::Disabled
        })
    }) && let Err(reply) = enablement::finish_worker(router, id, Duration::from_secs(3))
    {
        return reply;
    }
    let (record, complete) = match removed::prepare(
        state,
        device.account_id,
        id,
        &req,
        router.storage.active_size(id),
    ) {
        Ok(v) => v,
        Err(e) => return error(e),
    };
    if complete {
        return remove_reply(&record);
    }
    let Some(jobs) = router.imports() else {
        return failure(StatusCode::SERVICE_UNAVAILABLE, "unavailable");
    };
    if let Err(error) = removed::check_removable_path(jobs.servers_root(), &record) {
        return self::error(error);
    }
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
        // Keep the current durable policy while shutdown can still fail. Host
        // admission is locked here, and finish revokes Host access on commit.
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
    router
        .workers
        .lock()
        .unwrap()
        .retain(|w| w.server_id() != id);
    if let Err(e) = removed::finish(state, jobs.servers_root(), &record, &req) {
        return error(e);
    }
    router.storage.refresh();
    remove_reply(&record)
}
pub(super) fn error(e: JobError) -> Reply {
    let status = match e.0 {
        "forbidden" => StatusCode::FORBIDDEN,
        "not_found" => StatusCode::NOT_FOUND,
        "gone" => StatusCode::GONE,
        "invalid_request" => StatusCode::BAD_REQUEST,
        "confirm_name_mismatch" => StatusCode::UNPROCESSABLE_ENTITY,
        "server_busy"
        | "idempotency_conflict"
        | "purging"
        | "game_port_in_use"
        | "port_state_unknown"
        | "port_unresolved"
        | "destination_conflict" => StatusCode::CONFLICT,
        _ => StatusCode::SERVICE_UNAVAILABLE,
    };
    failure(status, e.0)
}
