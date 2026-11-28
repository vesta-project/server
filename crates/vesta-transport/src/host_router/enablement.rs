use super::access::{access_error, version};
use super::*;
use vesta_control::{
    machine::{registry::ServerState, removed},
    worker::WorkerShutdown,
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Patch {
    expected_version: String,
    enabled: bool,
}

fn busy(code: &'static str) -> Reply {
    failure(StatusCode::CONFLICT, code)
}

fn unavailable() -> Reply {
    failure(StatusCode::SERVICE_UNAVAILABLE, "worker_unavailable")
}

pub(super) fn dispatch(
    router: &HostRouter,
    device: &Device,
    server: Uuid,
    method: &Method,
    query: Option<&str>,
    bytes: &[u8],
) -> Reply {
    let result = (|| {
        if query.is_some() {
            return Err(failure(StatusCode::BAD_REQUEST, "invalid_request"));
        }
        let state = &router.identity.state;
        let actor = Actor::Account(device.account_id);
        if method == Method::GET && bytes.is_empty() {
            let role = state
                .effective_role(device.account_id, server)
                .map_err(access_error)?
                .ok_or_else(|| failure(StatusCode::NOT_FOUND, "not_found"))?;
            let caps = state
                .server_capabilities(server, device.account_id, role)
                .map_err(access_error)?;
            let entry = state
                .server(server)
                .map_err(access_error)?
                .ok_or_else(|| failure(StatusCode::NOT_FOUND, "not_found"))?;
            let revision = state.snapshot().map_err(access_error)?.revision;
            return Ok(crate::manage::ok(json!({
                "version":revision.to_string(),
                "enabled":entry.state != ServerState::Disabled,
                "can_manage":caps.contains(&vesta_control::identity::Capability::LifecycleEnable),
            })));
        }
        if method != Method::PATCH {
            return Err(failure(StatusCode::BAD_REQUEST, "invalid_request"));
        }
        let input: Patch = parse(bytes)?;
        let expected = version(&input.expected_version)?;
        state
            .authorize_enablement(actor, server, expected)
            .map_err(access_error)?;
        let entry = state
            .server(server)
            .map_err(access_error)?
            .ok_or_else(|| failure(StatusCode::NOT_FOUND, "not_found"))?;
        let enabled = entry.state != ServerState::Disabled;
        if router.is_stopping() || removed::has_pending(state, server).map_err(|_| unavailable())? {
            return Err(busy("server_busy"));
        }

        if enabled && !input.enabled {
            let owners = router.owned.lock().map_err(|_| unavailable())?;
            if let Some(worker) = owners
                .iter()
                .find(|worker| worker.handle().server_id() == server)
            {
                let _automation = worker.disable_guard().map_err(busy)?;
                if let Some(code) = worker.disable_blocker().map_err(|_| unavailable())? {
                    return Err(busy(code));
                }
                state
                    .set_server_enabled(actor, server, false, Some(expected))
                    .map_err(access_error)?;
                if worker.mark_disabled().is_err() {
                    let _ = state.set_server_enabled(Actor::LocalRecovery, server, true, None);
                    return Err(unavailable());
                }
                worker
                    .request_system_shutdown()
                    .map_err(|_| unavailable())?;
            } else {
                if router
                    .workers
                    .lock()
                    .map_err(|_| unavailable())?
                    .iter()
                    .any(|worker| worker.server_id() == server)
                {
                    return Err(unavailable());
                }
                state
                    .set_server_enabled(actor, server, false, Some(expected))
                    .map_err(access_error)?;
            }
        }
        if !input.enabled || !enabled {
            finish_worker(router, server, Duration::from_secs(3))?;
        }
        if input.enabled && !enabled {
            let factory = router.worker_factory(server).map_err(|_| unavailable())?;
            state
                .set_server_enabled(actor, server, true, Some(expected))
                .map_err(access_error)?;
            match factory(server, true) {
                Ok(worker) => {
                    router.adopt_worker(worker);
                }
                Err(error) => {
                    let _ = state.set_server_enabled(Actor::LocalRecovery, server, false, None);
                    let _ = state.record_startup_open_failure(server, error.code());
                    return Err(unavailable());
                }
            }
        }
        let revision = state.snapshot().map_err(access_error)?.revision;
        Ok(crate::manage::ok(
            json!({"version":revision.to_string(),"enabled":input.enabled}),
        ))
    })();
    result.unwrap_or_else(|reply| reply)
}

/// The caller holds the host's server admission lock. A timed-out worker
/// remains owned but is unrouted, and subsequent requests can finish the drain.
pub(super) fn finish_worker(
    router: &HostRouter,
    server: Uuid,
    timeout: Duration,
) -> Result<(), Reply> {
    let worker = {
        let mut owners = router.owned.lock().map_err(|_| unavailable())?;
        owners
            .iter()
            .position(|worker| worker.handle().server_id() == server)
            .map(|index| owners.remove(index))
    };
    router
        .workers
        .lock()
        .map_err(|_| unavailable())?
        .retain(|worker| worker.server_id() != server);
    if let Some(worker) = worker {
        match worker.shutdown_system(timeout).map_err(|_| unavailable())? {
            WorkerShutdown::Complete => {}
            WorkerShutdown::TimedOut(worker) => {
                router
                    .owned
                    .lock()
                    .map_err(|_| unavailable())?
                    .push(*worker);
                return Err(busy("server_disable_pending"));
            }
        }
    }
    Ok(())
}

pub(super) fn reap(router: &HostRouter) {
    let Ok(_admission) = router.removal_lock.try_lock() else {
        return;
    };
    let Ok(owners) = router.owned.lock() else {
        return;
    };
    let disabled: Vec<_> =
        owners
            .iter()
            .map(|worker| worker.handle().server_id())
            .filter(|&id| {
                router.identity.state.server(id).is_ok_and(|entry| {
                    entry.is_some_and(|entry| entry.state == ServerState::Disabled)
                })
            })
            .collect();
    drop(owners);
    for id in disabled {
        let _ = finish_worker(router, id, Duration::ZERO);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use vesta_control::{
        config::HostConfiguration,
        machine::{
            HostState,
            registry::{ServerEntry, Visibility},
            startup::StartupMode,
        },
    };

    #[test]
    fn req_disabled_worker_is_retired_skipped_at_boot_and_reopened_on_enable() {
        let temp = tempfile::tempdir().unwrap();
        let base = temp.path();
        let clock = Arc::new(vesta_state::SystemClock);
        let state = HostState::open(base.join("host"), clock.clone()).unwrap();
        let device = state.claim(Uuid::new_v4(), "Owner", [4; 32]).unwrap();
        let actor = Actor::Account(device.account_id);
        let server = Uuid::new_v4();
        state
            .register_server(
                actor,
                &ServerEntry {
                    server_id: server,
                    name: "Test worker".into(),
                    game_port: 31147,
                    state: ServerState::Stopped,
                    visibility: Visibility::Private,
                },
                state.snapshot().unwrap().revision,
            )
            .unwrap();
        for dir in ["game/config", "private", "bin"] {
            std::fs::create_dir_all(base.join(dir)).unwrap();
        }
        let executable = base.join("bin/inert");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::write(&executable, b"#!/bin/sh\nwhile IFS= read -r line; do\n  if [ \"$line\" = stop ]; then echo stopped > stopped-by-console; exit 0; fi\ndone\n").unwrap();
            std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        #[cfg(not(unix))]
        std::fs::write(&executable, b"").unwrap();
        std::fs::write(base.join("game/marker"), b"keep").unwrap();
        std::fs::write(
            base.join("private/launch.toml"),
            "[minecraft]
working-directory='game'
[launch]
kind='native'
executable='../bin/inert'
stop='console-stop'
",
        )
        .unwrap();
        let config = "schema-version=1
[paths]
state='worker-state'
game='game'
scan-roots=['game/config']
launch-definition='private/launch.toml'
[pack]
name='Fixture'
version='1.0.0'
revision=1
minimum-revision=1
minecraft-version='1.21.1'
unresolved-disposition='embed-public'
[pack-access]
mode='minecraft'
[listener]
mode='disabled'
";
        let host_state = state.clone();
        state
            .patch_startup(
                actor,
                server,
                state.snapshot().unwrap().revision,
                Default::default(),
                Some(vesta_control::machine::startup::StartupAction::StayStopped),
                None,
            )
            .unwrap();
        let opened = Arc::new(AtomicUsize::new(0));
        let starts = opened.clone();
        let worker_base = base.to_owned();
        let router = HostRouter::new(
            Arc::new(HostIdentity::new(state.clone()).unwrap()),
            "Fixture".into(),
            Arc::new(FolderBrowser::new(vec![], vec![base.join("host")]).unwrap()),
        )
        .with_configured_worker(server, move |id, boot| {
            assert!(boot);
            starts.fetch_add(1, Ordering::SeqCst);
            let config = HostConfiguration::parse_toml(
                config.as_bytes(),
                &worker_base,
                std::path::Path::new("host.toml"),
            )
            .unwrap();
            let snapshot = host_state.snapshot().unwrap();
            let startup = StartupMode::Managed(host_state.startup_settings(id).unwrap().unwrap());
            Ok(Worker::open_with_startup(
                config,
                clock.clone(),
                None,
                snapshot.host_id,
                id,
                startup,
            )
            .unwrap())
        });
        assert!(router.restore_registered_workers().is_empty());
        assert_eq!(opened.load(Ordering::SeqCst), 1);
        let change = |enabled| {
            let version = state.snapshot().unwrap().revision.to_string();
            dispatch(
                &router,
                &device,
                server,
                &Method::PATCH,
                None,
                &serde_json::to_vec(&json!({"expected_version":version,"enabled":enabled}))
                    .unwrap(),
            )
        };
        let handle = router.owned_handle(server).unwrap();
        let subject = subject(&router, &handle, &device)
            .unwrap_or_else(|reply| panic!("subject failed: {}", reply.status()));
        let net = handle.authorize(subject).unwrap();
        for (kind, code) in [
            ("backup", "backup_job_active"),
            ("restore", "restore_job_active"),
        ] {
            net.database().write(|c| {
                c.execute("INSERT INTO backup_jobs(id,profile,kind,state,requested_by,requested_device,created_at)
                    VALUES ('job','default',?,'running','fixture','fixture',0)", [kind])?;
                Ok(())
            }).unwrap();
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            let status = loop {
                assert!(std::time::Instant::now() < deadline);
                let response = change(false);
                let status = response.status();
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .build()
                    .unwrap();
                let body = runtime
                    .block_on(response.into_body().collect())
                    .unwrap()
                    .to_bytes();
                let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
                if value["error"]["code"] == "server_busy" {
                    std::thread::sleep(Duration::from_millis(10));
                    continue;
                }
                assert_eq!(value["error"]["code"], code);
                break status;
            };
            assert_eq!(status, StatusCode::CONFLICT);
            assert_eq!(
                state.server(server).unwrap().unwrap().state,
                ServerState::Stopped
            );
            net.database()
                .write(|c| {
                    c.execute("DELETE FROM backup_jobs WHERE id='job'", [])?;
                    Ok(())
                })
                .unwrap();
        }
        #[cfg(unix)]
        {
            use vesta_control::{
                identity::{DeviceId, PrincipalId, Subject},
                operations::GameCommand,
            };
            let subject = Subject {
                principal: PrincipalId::parse(&device.account_id.to_string()).unwrap(),
                device: DeviceId::parse(&device.device_id.to_string()).unwrap(),
            };
            net.runtime()
                .submit_game(&subject, GameCommand::Start, "fixture-before-disable")
                .unwrap();
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            while net.runtime().game_snapshot().unwrap().generation.is_none() {
                assert!(std::time::Instant::now() < deadline);
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        let until = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            if change(false).status() == StatusCode::OK {
                break;
            }
            assert!(std::time::Instant::now() < until);
            std::thread::sleep(Duration::from_millis(10));
        }
        #[cfg(unix)]
        assert_eq!(
            std::fs::read(base.join("game/stopped-by-console")).unwrap(),
            b"stopped\n"
        );
        assert!(router.owned_handle(server).is_none());
        assert_eq!(
            state.server(server).unwrap().unwrap().state,
            ServerState::Disabled
        );
        assert_eq!(
            router.import_worker(server).err().unwrap().code(),
            "server_disabled"
        );
        assert!(router.restore_registered_workers().is_empty());
        assert_eq!(opened.load(Ordering::SeqCst), 1);
        assert_eq!(std::fs::read(base.join("game/marker")).unwrap(), b"keep");
        assert_eq!(change(true).status(), StatusCode::OK);
        assert_eq!(opened.load(Ordering::SeqCst), 2);
        assert!(router.owned_handle(server).is_some());
        assert_eq!(
            state.server(server).unwrap().unwrap().state,
            ServerState::Stopped
        );
        let report_deadline = std::time::Instant::now() + Duration::from_secs(3);
        let worker = router.owned_handle(server).unwrap();
        loop {
            if worker.startup_report().is_some_and(|report| {
                matches!(
                    report.outcome,
                    vesta_control::machine::startup::StartupOutcome::Applied
                )
            }) {
                break;
            }
            assert!(std::time::Instant::now() < report_deadline);
            std::thread::sleep(Duration::from_millis(20));
        }
        for worker in router.take_owned_workers() {
            assert!(matches!(
                worker.shutdown_system(Duration::from_secs(3)).unwrap(),
                WorkerShutdown::Complete
            ));
        }
    }
}
