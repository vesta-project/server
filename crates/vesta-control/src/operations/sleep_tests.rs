use super::*;
use crate::operations::{OperationState, StartGate};
use crate::{
    motd::State,
    sleep::{Coordinator, Policy, Sleep, SleepBlocker},
};
fn address() -> std::net::SocketAddr {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    l.local_addr().unwrap()
}
fn service(h: &Harness, a: std::net::SocketAddr, gate: StartGate) -> Sleep {
    Sleep::new(
        h.db.clone(),
        gate,
        Some(a),
        Some(a),
        "Owned sleep fixture".into(),
    )
}
fn owned(h: &Harness, a: std::net::SocketAddr, gate: StartGate) -> OwnedRuntime {
    fs::write(h.cwd.join("sleep-status-address"), a.to_string()).unwrap();
    OwnedRuntime {
        runtime: OperationRuntime::open_with_readiness(
            h.operations.clone(),
            h.launch(),
            vesta_game_runtime::readiness::ReadinessConfig {
                probe: Some(a),
                probe_interval: Duration::from_millis(100),
                probe_timeout: Duration::from_millis(100),
                ..Default::default()
            },
            gate,
        )
        .unwrap(),
        cwd: h.cwd.clone(),
        gate_release: None,
    }
}
#[test]
fn sleep_policy_is_off_revisioned_audited_and_revocable_without_panics() {
    let h = Harness::new(false);
    let a = address();
    let s = service(&h, a, StartGate::default());
    assert_eq!(s.policy(&h.subject).unwrap().revision, 0);
    assert!(!s.policy(&h.subject).unwrap().policy.enabled);
    assert_eq!(
        s.policy(&h.subject).unwrap().sleep_blocker,
        SleepBlocker::Disabled
    );
    let policy = Policy {
        enabled: true,
        offline_status: true,
        wake_on_join: true,
        ..Policy::default()
    };
    assert_eq!(
        s.configure(&h.subject, 0, policy.clone()).unwrap().revision,
        1
    );
    assert_eq!(
        s.configure(&h.subject, 0, policy.clone()).err(),
        Some("sleep_policy_stale_revision")
    );
    h.db.write(|c|{c.execute_batch("CREATE TRIGGER fail_sleep_audit BEFORE INSERT ON audit_events WHEN NEW.action='sleep.policy.configure' BEGIN SELECT RAISE(ABORT,'fixture'); END;")?;Ok(())}).unwrap();
    assert!(s.configure(&h.subject, 1, Policy::default()).is_err());
    assert_eq!(s.policy(&h.subject).unwrap().revision, 1);
    h.db.write(|c|{c.execute_batch("DROP TRIGGER fail_sleep_audit")?;c.execute("INSERT INTO principal_overrides(id,principal_id,capability,scope,effect,granted_by,created_at) VALUES(?,?,'lifecycle.start','*','deny','fixture',?)",params![uuid::Uuid::new_v4().to_string(),h.subject.principal.as_str(),h.db.clock().millis()])?;Ok(())}).unwrap();
    assert_eq!(
        s.configure(&h.subject, 1, policy).err(),
        Some("sleep_forbidden")
    );
}

#[test]
fn disabled_sleep_uses_idle_cadence_but_offline_and_active_sleep_keep_fast_cadence() {
    let h = Harness::new(false);
    let a = address();
    let gate = StartGate::default();
    let s = service(&h, a, gate.clone());
    let owned = owned(&h, a, gate);
    let runtime = owned.runtime.network_handle();
    let mut coordinator = Coordinator::default();
    coordinator.tick_at(&s, &runtime, Instant::now()).unwrap();
    assert_eq!(coordinator.poll_interval(), Duration::from_secs(1));
    let mut revision = 0;
    for policy in [
        Policy {
            enabled: true,
            ..Policy::default()
        },
        Policy {
            offline_status: true,
            ..Policy::default()
        },
        Policy::default(),
    ] {
        let active = policy.enabled || policy.offline_status;
        revision = s.configure(&h.subject, revision, policy).unwrap().revision;
        coordinator.tick_at(&s, &runtime, Instant::now()).unwrap();
        assert_eq!(
            coordinator.poll_interval(),
            if active {
                Duration::from_millis(250)
            } else {
                Duration::from_secs(1)
            }
        );
    }
}
#[test]
fn sleep_reconciles_zero_waits_for_warning_then_gracefully_stops_and_marks_sleeping() {
    let h = Harness::new(false);
    let a = address();
    let gate = StartGate::default();
    let s = service(&h, a, gate.clone());
    let owned = owned(&h, a, gate);
    let runtime = owned.runtime.network_handle();
    start(&h, &owned.runtime);
    wait_for(|| {
        runtime.readiness().unwrap().probed
            && runtime.readiness().unwrap().readiness
                == crate::operations::readiness::Readiness::Ready
    });
    s.configure(
        &h.subject,
        0,
        Policy {
            enabled: true,
            idle_seconds: 60,
            warning_seconds: 5,
            ..Policy::default()
        },
    )
    .unwrap();
    let mut c = Coordinator::default();
    let base = Instant::now();
    c.tick_at(&s, &runtime, base).unwrap();
    assert_eq!(
        s.policy(&h.subject).unwrap().sleep_blocker,
        SleepBlocker::CountingDown {
            remaining_seconds: 60
        }
    );
    c.tick_at(&s, &runtime, base + Duration::from_secs(61))
        .unwrap();
    wait_for(|| {
        fs::read_to_string(h.cwd.join("operation-commands"))
            .unwrap_or_default()
            .contains("say Server will sleep")
    });
    // Seeing the child consume stdin does not prove the operation committed.
    wait_for(|| {
        h.db.read(|c|Ok(c.query_row("SELECT EXISTS(SELECT 1 FROM operations WHERE server_id=? AND type='game.console' AND state='succeeded')",[h.db.server_id()],|r|r.get::<_,bool>(0))?)).unwrap()
    });
    c.tick_at(&s, &runtime, base + Duration::from_secs(62))
        .unwrap();
    assert_eq!(
        s.sleep_blocker().unwrap(),
        SleepBlocker::WarningSent {
            remaining_seconds: 5
        }
    );
    assert!(
        runtime.game_snapshot().unwrap().generation.is_some(),
        "warning acknowledgement starts countdown; elapsed idle is insufficient"
    );
    c.tick_at(&s, &runtime, base + Duration::from_secs(73))
        .unwrap();
    wait_for(|| runtime.game_snapshot().unwrap().generation.is_none());
    // Native process exit can precede the durable sleep result. Reconciliation
    // must observe that result before the public state becomes Sleeping.
    wait_for(|| {
        c.tick_at(&s, &runtime, base + Duration::from_secs(74))
            .unwrap();
        s.public_state(&runtime).unwrap() == State::Sleeping
    });
    assert_eq!(s.public_state(&runtime).unwrap(), State::Sleeping);
    assert_eq!(
        fs::read_to_string(h.cwd.join("operation-commands"))
            .unwrap()
            .lines()
            .filter(|v| *v == "stop")
            .count(),
        1
    );
}
#[test]
fn sleep_unknown_player_reconciliation_never_stops_even_after_idle_horizon() {
    let h = Harness::new(false);
    let a = address();
    let gate = StartGate::default();
    let s = service(&h, a, gate.clone());
    let owned = owned(&h, a, gate);
    let runtime = owned.runtime.network_handle();
    fs::write(h.cwd.join("sleep-no-list"), b"missing response").unwrap();
    start(&h, &owned.runtime);
    wait_for(|| {
        runtime.readiness().unwrap().readiness == crate::operations::readiness::Readiness::Ready
    });
    s.configure(
        &h.subject,
        0,
        Policy {
            enabled: true,
            idle_seconds: 60,
            warning_seconds: 5,
            ..Policy::default()
        },
    )
    .unwrap();
    let mut c = Coordinator::default();
    let base = Instant::now();
    c.tick_at(&s, &runtime, base).unwrap();
    c.tick_at(&s, &runtime, base + Duration::from_secs(100))
        .unwrap();
    assert_eq!(s.sleep_blocker().unwrap(), SleepBlocker::PlayerCountUnknown);
    assert_eq!(
        fs::read_to_string(h.cwd.join("operation-commands"))
            .unwrap()
            .lines()
            .filter(|line| *line == "list")
            .count(),
        1
    );
    assert!(runtime.game_snapshot().unwrap().generation.is_some());
    assert!(
        !fs::read_to_string(h.cwd.join("operation-commands"))
            .unwrap()
            .contains("stop")
    );
}
#[test]
fn sleep_login_burst_coalesces_to_durable_start_and_hands_port_to_actual_child() {
    use std::io::Write;
    let h = Harness::new(false);
    let a = address();
    let gate = StartGate::default();
    let s = service(&h, a, gate.clone());
    let owned = owned(&h, a, gate);
    let runtime = owned.runtime.network_handle();
    s.configure(
        &h.subject,
        0,
        Policy {
            offline_status: true,
            wake_on_join: true,
            ..Policy::default()
        },
    )
    .unwrap();
    let mut c = Coordinator::default();
    let base = Instant::now();
    c.tick_at(&s, &runtime, base).unwrap();
    wait_for(|| std::net::TcpStream::connect(a).is_ok());
    for _ in 0..8 {
        let mut peer = std::net::TcpStream::connect(a).unwrap();
        let mut handshake = vec![16, 0, 255, 5, 9];
        handshake.extend(b"localhost");
        handshake.extend(a.port().to_be_bytes());
        handshake.push(2);
        let mut login = vec![22, 0, 4];
        login.extend(b"Alex");
        login.extend([0; 16]);
        handshake.extend(login);
        let _ = peer.write_all(&handshake);
    }
    let deadline = Instant::now() + Duration::from_secs(10);
    while runtime.game_snapshot().unwrap().generation.is_none() {
        c.tick_at(&s, &runtime, Instant::now()).unwrap();
        assert!(
            Instant::now() < deadline,
            "valid login wake must admit start"
        );
        std::thread::sleep(Duration::from_millis(30));
    }
    wait_for(|| h.cwd.join("operation-ready").exists());
    let first = h
        .operations
        .find_key_trusted(&h.subject.principal, "sleep-wake:1")
        .unwrap()
        .unwrap();
    h.wait_state(&first.id, OperationState::Succeeded);
    assert!(
        h.operations
            .find_key_trusted(&h.subject.principal, "sleep-wake:2")
            .unwrap()
            .is_none()
    );
    // The child fixture binds the same TCP port before publishing ready. It would
    // fail startup if the temporary responder still retained its listening socket.
    assert!(vesta_game_runtime::readiness::status_probe(a, Duration::from_secs(1)).is_ok());
}
#[test]
fn sleep_join_and_incompatible_hold_reset_idle_and_cancel_countdown() {
    let h = Harness::new(false);
    let a = address();
    let gate = StartGate::default();
    let s = service(&h, a, gate.clone());
    let owned = owned(&h, a, gate.clone());
    let runtime = owned.runtime.network_handle();
    start(&h, &owned.runtime);
    wait_for(|| {
        runtime.readiness().unwrap().readiness == crate::operations::readiness::Readiness::Ready
    });
    s.configure(
        &h.subject,
        0,
        Policy {
            enabled: true,
            idle_seconds: 60,
            warning_seconds: 5,
            ..Policy::default()
        },
    )
    .unwrap();
    let mut c = Coordinator::default();
    let base = Instant::now();
    c.tick_at(&s, &runtime, base).unwrap();
    let hold = gate.block();
    c.tick_at(&s, &runtime, base + Duration::from_secs(100))
        .unwrap();
    assert_eq!(s.sleep_blocker().unwrap(), SleepBlocker::Hold);
    drop(hold);
    assert!(
        !fs::read_to_string(h.cwd.join("operation-commands"))
            .unwrap()
            .contains("say Server will sleep")
    );
    c.tick_at(&s, &runtime, base + Duration::from_secs(101))
        .unwrap();
    c.tick_at(&s, &runtime, base + Duration::from_secs(163))
        .unwrap();
    wait_for(|| {
        fs::read_to_string(h.cwd.join("operation-commands"))
            .unwrap()
            .contains("say Server will sleep")
    });
    runtime
        .shared
        .console
        .ingest(
            vesta_game_runtime::console::ConsoleStream::Stdout,
            "[12:00:03] [Server thread/INFO]: System chat: Alex joined the game",
            false,
        )
        .unwrap();
    c.tick_at(&s, &runtime, base + Duration::from_secs(174))
        .unwrap();
    std::thread::sleep(Duration::from_millis(30));
    c.tick_at(&s, &runtime, base + Duration::from_secs(200))
        .unwrap();
    assert_eq!(s.sleep_blocker().unwrap(), SleepBlocker::PlayersOnline);
    assert!(runtime.game_snapshot().unwrap().generation.is_some());
    assert!(
        !fs::read_to_string(h.cwd.join("operation-commands"))
            .unwrap()
            .lines()
            .any(|line| line == "stop")
    );
}
#[test]
fn sleep_native_intent_checks_reject_disabled_or_rebound_policy_and_wrong_keys() {
    let h = Harness::new(false);
    let s = service(&h, address(), StartGate::default());
    let policy = Policy {
        enabled: true,
        wake_on_join: true,
        offline_status: true,
        ..Policy::default()
    };
    s.configure(&h.subject, 0, policy).unwrap();
    h.db.write(|c|{c.execute("UPDATE settings SET desired_value=? WHERE server_id=? AND key='sleep-runtime'",params![serde_json::json!({"sleeping":false,"sleep_key":"sleep-stop:owned","wake_key":"sleep-wake:1","cooldown_until_ms":0,"sequence":1}).to_string(),h.db.server_id()])?;Ok(())}).unwrap();
    assert!(
        h.db.read(|c| crate::sleep::permit_operation_in(
            c,
            h.db.server_id(),
            &h.subject,
            Some("sleep-wake:1"),
            true
        ))
        .is_ok()
    );
    assert!(
        h.db.read(|c| crate::sleep::permit_operation_in(
            c,
            h.db.server_id(),
            &h.subject,
            Some("other"),
            true
        ))
        .is_err()
    );
    let mut stranger = h.subject.clone();
    stranger.device = DeviceId::parse(&uuid::Uuid::new_v4().to_string()).unwrap();
    assert!(
        h.db.read(|c| crate::sleep::permit_operation_in(
            c,
            h.db.server_id(),
            &stranger,
            Some("sleep-wake:1"),
            true
        ))
        .is_err()
    );
    s.configure(&h.subject, 1, Policy::default()).unwrap();
    assert!(
        h.db.read(|c| crate::sleep::permit_operation_in(
            c,
            h.db.server_id(),
            &h.subject,
            Some("sleep-wake:1"),
            true
        ))
        .is_err()
    );
    assert!(
        h.db.read(|c| crate::sleep::permit_operation_in(
            c,
            h.db.server_id(),
            &h.subject,
            Some("sleep-stop:owned"),
            false
        ))
        .is_err()
    );
}
#[test]
fn sleep_wake_rechecks_revoked_start_authority_after_network_hint_and_keeps_status() {
    use std::io::{Read, Write};
    let h = Harness::new(false);
    let a = address();
    let gate = StartGate::default();
    let s = service(&h, a, gate.clone());
    let owned = owned(&h, a, gate);
    let runtime = owned.runtime.network_handle();
    s.configure(
        &h.subject,
        0,
        Policy {
            offline_status: true,
            wake_on_join: true,
            ..Policy::default()
        },
    )
    .unwrap();
    let mut c = Coordinator::default();
    c.tick_at(&s, &runtime, Instant::now()).unwrap();
    wait_for(|| std::net::TcpStream::connect(a).is_ok());
    let mut peer = std::net::TcpStream::connect(a).unwrap();
    peer.set_read_timeout(Some(Duration::from_secs(1))).unwrap();
    let mut packet = vec![16, 0, 255, 5, 9];
    packet.extend(b"localhost");
    packet.extend(a.port().to_be_bytes());
    packet.extend([2, 22, 0, 4]);
    packet.extend(b"Alex");
    packet.extend([0; 16]);
    peer.write_all(&packet).unwrap();
    let mut reply = [0; 1024];
    assert!(peer.read(&mut reply).unwrap() > 0);
    h.db.write(|db|{db.execute("INSERT INTO principal_overrides(id,principal_id,capability,scope,effect,granted_by,created_at) VALUES(?,?,'lifecycle.start','*','deny','fixture',?)",params![uuid::Uuid::new_v4().to_string(),h.subject.principal.as_str(),h.db.clock().millis()])?;Ok(())}).unwrap();
    c.tick_at(&s, &runtime, Instant::now()).unwrap();
    std::thread::sleep(Duration::from_millis(50));
    c.tick_at(&s, &runtime, Instant::now()).unwrap();
    assert!(runtime.game_snapshot().unwrap().generation.is_none());
    assert!(
        h.operations
            .find_key_trusted(&h.subject.principal, "sleep-wake:1")
            .unwrap()
            .is_none()
    );
    assert!(vesta_game_runtime::readiness::status_probe(a, Duration::from_secs(1)).is_ok());
}
#[test]
fn sleep_elapsed_placeholder_tracks_observed_state_and_generation_monotonically() {
    let h = Harness::new(false);
    let s = service(&h, address(), StartGate::default());
    let base = Instant::now();
    assert_eq!(
        s.observed_elapsed_at(State::Offline, None, base).unwrap(),
        0
    );
    assert_eq!(
        s.observed_elapsed_at(State::Offline, None, base + Duration::from_secs(10))
            .unwrap(),
        10
    );
    assert_eq!(
        s.observed_elapsed_at(State::Sleeping, None, base + Duration::from_secs(11))
            .unwrap(),
        0
    );
    assert_eq!(
        s.observed_elapsed_at(State::Sleeping, None, base + Duration::from_secs(15))
            .unwrap(),
        4
    );
}
