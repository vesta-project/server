//! Reuses the owned inert child; metrics must never write `list` or any command.
use super::*;
#[test]
fn performance_capture_is_one_bounded_authorized_snapshot_without_console_work() {
    use crate::{
        config::MetricsSettings,
        metrics::{Fetch, Resolution},
    };
    let h = Harness::new(false);
    let settings = MetricsSettings::default();
    let request = Fetch {
        series: vec!["tps".into(), "game_memory_bytes".into()],
        from: 0,
        to: 1000,
        resolution: Resolution::Raw,
        max_points: 10,
        after: None,
    };
    let before = h
        .operations
        .metrics_current(&h.subject)
        .unwrap()
        .status
        .revision;
    let capture = h
        .operations
        .metrics_capture(&h.subject, &request, settings)
        .unwrap();
    assert_eq!(capture.schema, 1);
    assert_eq!(capture.source, "retained_telemetry");
    assert!(!capture.profiler_started);
    assert_eq!(capture.server_id, h.db.server_id());
    assert_eq!(capture.current.status.revision, capture.history.cursor);
    assert_eq!(capture.current.status.revision, before);
    assert!(capture.history.columns.timestamps.is_empty());
    assert!(capture.current.sample.is_none());
    assert_eq!(capture.request.series, request.series);
    assert!(!h.cwd.join("operation-commands").exists());
    let mut invalid = request.clone();
    invalid.after = Some(0);
    assert!(
        h.operations
            .metrics_capture(&h.subject, &invalid, settings)
            .is_err()
    );
    invalid = request.clone();
    invalid.max_points = crate::metrics::MAX_POINTS + 1;
    assert!(
        h.operations
            .metrics_capture(&h.subject, &invalid, settings)
            .is_err()
    );
    h.db.write(|c| {
        c.execute(
            "DELETE FROM role_assignments WHERE principal_id=?",
            [h.subject.principal.as_str()],
        )?;
        Ok(())
    })
    .unwrap();
    assert!(
        h.operations
            .metrics_capture(&h.subject, &request, settings)
            .is_err()
    );
    assert_eq!(
        h.db.read(|c| Ok(c.query_row(
            "SELECT revision FROM metrics_status WHERE singleton=1",
            [],
            |r| r.get::<_, i64>(0)
        )?))
        .unwrap(),
        before
    );
}

#[test]
fn metrics_policy_changes_invalidate_cached_and_incremental_projections_atomically() {
    use crate::{
        config::MetricsSettings,
        metrics::{Fetch, PolicyDocument, Resolution},
    };
    let harness = Harness::new(false);
    let settings = MetricsSettings::default();
    let before = harness
        .operations
        .metrics_current(&harness.subject)
        .unwrap()
        .status
        .revision;
    let mut document = PolicyDocument::host(settings);
    document.settings.sample_seconds = 5;
    harness
        .operations
        .set_metrics_policy(&harness.subject, None, document)
        .unwrap();
    let after = harness
        .operations
        .metrics_current(&harness.subject)
        .unwrap()
        .status
        .revision;
    assert_eq!(after, before + 1);
    let request = Fetch {
        series: vec!["tps".into()],
        from: 0,
        to: 1000,
        resolution: Resolution::Auto,
        max_points: 10,
        after: Some(before),
    };
    let fetched = harness
        .operations
        .metrics_fetch(&harness.subject, &request, settings)
        .unwrap();
    assert!(fetched.retention_gap);
    assert_eq!(fetched.cursor, after);
    // Failed audit persistence must roll back policy and projection revisions.
    harness.db.write(|c| {
        c.execute_batch("CREATE TRIGGER reject_metrics_policy_audit BEFORE INSERT ON audit_events WHEN NEW.action='metrics.policy.update' BEGIN SELECT RAISE(ABORT,'fixture'); END;")?;
        Ok(())
    }).unwrap();
    document.settings.sample_seconds = 10;
    assert!(
        harness
            .operations
            .set_metrics_policy(&harness.subject, Some(1), document)
            .is_err()
    );
    assert_eq!(
        harness
            .operations
            .metrics_current(&harness.subject)
            .unwrap()
            .status
            .revision,
        after
    );
    let policy = harness
        .operations
        .metrics_policy(&harness.subject, settings)
        .unwrap();
    assert_eq!(policy.revision, Some(1));
    assert_eq!(policy.document.settings.sample_seconds, 5);
}

#[test]
fn metrics_sampler_is_passive_generation_bound_and_freshly_authorized() {
    let harness = Harness::new(false);
    let owned = harness.runtime(Duration::from_secs(1));
    let generation = start(&harness, &owned.runtime);
    let handle = owned.runtime.network_handle();
    let identity = handle
        .metrics_identity()
        .expect("direct launch captures native creation identity");
    assert_eq!(identity.0, generation);
    assert_eq!(
        vesta_supervisor::native::process_start(identity.1),
        Some(identity.2)
    );
    let mut sampler = crate::metrics::Sampler::new(
        harness.db.clone(),
        crate::config::MetricsSettings::default(),
        harness.cwd.clone(),
        None,
    );
    sampler.tick(&handle, harness.db.clock().millis());
    let current = harness
        .operations
        .metrics_current(&harness.subject)
        .unwrap();
    let sample = current.sample.unwrap();
    assert_eq!(sample.generation, Some(generation.to_hex()));
    assert!(sample.values["game_memory_bytes"].is_some_and(|v| v > 0.0));
    assert!(sample.values["game_cpu_percent"].is_none());
    assert!(sample.values["players_online"].is_none());
    assert!(sample.values["tps"].is_none());
    assert_eq!(
        fs::read_to_string(harness.cwd.join("operation-commands")).unwrap(),
        ""
    );
    let events = harness
        .operations
        .player_activity(&harness.subject, 0, 100)
        .unwrap();
    assert_eq!(events[0].kind, "gap");
    harness
        .db
        .write(|c| {
            c.execute(
                "UPDATE role_assignments SET role_id='moderator' WHERE principal_id=?",
                [harness.subject.principal.as_str()],
            )?;
            Ok(())
        })
        .unwrap();
    assert_eq!(
        harness
            .operations
            .metrics_current(&harness.subject)
            .err()
            .unwrap()
            .code(),
        "forbidden"
    );
    assert!(
        harness
            .operations
            .player_activity(&harness.subject, 0, 100)
            .is_ok()
    );
    harness.db.write(|c| {
        c.execute("UPDATE role_assignments SET role_id='viewer' WHERE principal_id=?",[harness.subject.principal.as_str()])?;
        c.execute("INSERT INTO principal_overrides(id,principal_id,capability,scope,effect,granted_by,created_at) VALUES(?,?,'players.view','*','deny','fixture',?)",params![uuid::Uuid::new_v4().to_string(),harness.subject.principal.as_str(),harness.db.clock().millis()])?;Ok(())
    }).unwrap();
    assert!(harness.operations.metrics_current(&harness.subject).is_ok());
    assert_eq!(
        harness
            .operations
            .player_activity(&harness.subject, 0, 100)
            .err()
            .unwrap()
            .code(),
        "forbidden"
    );
    fs::write(harness.cwd.join("release-game"), b"release").unwrap();
    wait_for(|| handle.game_snapshot().unwrap().generation.is_none());
    assert!(handle.metrics_identity().is_none());
}

#[test]
fn metrics_stored_policy_is_revisioned_audited_authorized_and_picked_up_without_restart() {
    use crate::{config::MetricsSettings, metrics::PolicyDocument};
    let harness = Harness::new(false);
    let owned = harness.runtime(Duration::from_secs(1));
    let runtime = owned.runtime.network_handle();
    let fallback = MetricsSettings::default();
    let host = harness
        .operations
        .metrics_policy(&harness.subject, fallback)
        .unwrap();
    assert_eq!(host.source, "host_config");
    assert_eq!(host.revision, None);
    assert!(host.command_polling_writes_console);
    let mut document = PolicyDocument::host(fallback);
    document.settings.enabled = false;
    assert_eq!(
        harness
            .operations
            .set_metrics_policy(&harness.subject, None, document)
            .unwrap(),
        1
    );
    let mut sampler =
        crate::metrics::Sampler::new(harness.db.clone(), fallback, harness.cwd.clone(), None);
    let at = harness.db.clock().millis();
    sampler.tick(&runtime, at);
    assert!(
        harness
            .operations
            .metrics_current(&harness.subject)
            .unwrap()
            .sample
            .is_none()
    );
    assert_eq!(
        harness
            .operations
            .set_metrics_policy(&harness.subject, None, document)
            .unwrap_err()
            .code(),
        "source_revision_unavailable"
    );
    document.settings.enabled = true;
    document.settings.sample_seconds = 5;
    assert_eq!(
        harness
            .operations
            .set_metrics_policy(&harness.subject, Some(1), document)
            .unwrap(),
        2
    );
    sampler.tick(&runtime, at + 100);
    assert_eq!(
        harness
            .operations
            .metrics_current(&harness.subject)
            .unwrap()
            .sample
            .unwrap()
            .sampled_at,
        at + 100
    );
    let stored = harness
        .operations
        .metrics_policy(&harness.subject, fallback)
        .unwrap();
    assert_eq!(stored.source, "stored");
    assert_eq!(stored.document.settings.sample_seconds, 5);
    assert!(stored.storage_bytes > 0);
    document.settings.enabled = false;
    harness
        .operations
        .set_metrics_policy(&harness.subject, Some(2), document)
        .unwrap();
    sampler.tick(&runtime, at + 200);
    assert_eq!(
        harness
            .operations
            .metrics_current(&harness.subject)
            .unwrap()
            .sample
            .unwrap()
            .sampled_at,
        at + 100
    );
    let audit=harness.db.read(|c|Ok(c.query_row("SELECT COUNT(*) FROM audit_events WHERE action='metrics.policy.update' AND outcome='accepted'",[],|r|r.get::<_,i64>(0))?)).unwrap();
    assert_eq!(audit, 3);
    harness
        .db
        .write(|c| {
            c.execute(
                "UPDATE role_assignments SET role_id='moderator' WHERE principal_id=?",
                [harness.subject.principal.as_str()],
            )?;
            Ok(())
        })
        .unwrap();
    assert_eq!(
        harness
            .operations
            .set_metrics_policy(&harness.subject, Some(3), document)
            .unwrap_err()
            .code(),
        "forbidden"
    );
}
#[test]
fn metrics_live_snapshots_filter_player_identities_and_bound_latest_activity() {
    let harness = Harness::new(false);
    let at = harness.db.clock().millis();
    assert!(
        harness
            .operations
            .metrics_live(&harness.subject, None)
            .unwrap()
            .activity
            .is_empty()
    );
    harness.db.write(|c| {for n in 1..=40 {c.execute("INSERT INTO metrics_activity(id,at,generation,name,kind,bytes) VALUES(?,?,'00112233445566778899aabbccddeeff',?,'join',100)",params![n,at,format!("Private{n}")])?;}Ok(())}).unwrap();
    let live = harness
        .operations
        .metrics_live(&harness.subject, Some(0))
        .unwrap();
    assert_eq!(live.activity.len(), 32);
    assert_eq!(live.activity[0].id, 9);
    assert_eq!(live.activity_cursor, 40);
    assert!(live.activity_gap);
    harness.db.write(|c| {c.execute("INSERT INTO principal_overrides(id,principal_id,capability,scope,effect,granted_by,created_at) VALUES(?,?,'players.view','*','deny','fixture',?)",params![uuid::Uuid::new_v4().to_string(),harness.subject.principal.as_str(),at])?;Ok(())}).unwrap();
    let filtered = harness
        .operations
        .metrics_live(&harness.subject, Some(0))
        .unwrap();
    assert!(filtered.activity.is_empty());
    assert!(!filtered.activity_gap);
    harness.db.write(|c| {c.execute("INSERT INTO principal_overrides(id,principal_id,capability,scope,effect,granted_by,created_at) VALUES(?,?,'metrics.view','*','deny','fixture',?)",params![uuid::Uuid::new_v4().to_string(),harness.subject.principal.as_str(),at])?;Ok(())}).unwrap();
    assert_eq!(
        harness
            .operations
            .metrics_live(&harness.subject, Some(0))
            .err()
            .unwrap()
            .code(),
        "forbidden"
    );
}
#[test]
fn metrics_watch_coalesces_slow_consumers_to_latest_without_a_queue() {
    let harness = Harness::new(false);
    let owned = harness.runtime(Duration::from_secs(1));
    let runtime = owned.runtime.network_handle();
    let mut watch = runtime.metrics_notifications();
    for revision in 1..=10_000 {
        runtime.metrics_published(revision);
    }
    assert!(watch.has_changed().unwrap());
    assert_eq!(*watch.borrow_and_update(), 10_000);
    assert!(!watch.has_changed().unwrap());
    runtime.metrics_published(10_000);
    assert!(!watch.has_changed().unwrap());
}
#[test]
fn metrics_tick_polling_is_opt_in_ready_bound_correlated_and_stops_on_policy_disable() {
    use crate::{
        config::MetricsSettings,
        metrics::{
            PolicyDocument,
            ticks::{Adapter, Detection},
        },
    };
    let harness = Harness::new(false);
    fs::write(harness.cwd.join("metrics-tick-fixture"), b"enabled").unwrap();
    let owned = harness.runtime(Duration::from_secs(1));
    let runtime = owned.runtime.network_handle();
    let mut sampler = crate::metrics::Sampler::new(
        harness.db.clone(),
        MetricsSettings::default(),
        harness.cwd.clone(),
        None,
    )
    .with_tick_detection(Detection::fixture(Adapter::Spigot));
    let at = harness.db.clock().millis();
    sampler.tick(&runtime, at);
    assert!(
        harness
            .operations
            .metrics_current(&harness.subject)
            .unwrap()
            .sample
            .unwrap()
            .values["tps"]
            .is_none()
    );
    let mut document = PolicyDocument::host(MetricsSettings {
        sample_seconds: 5,
        ..MetricsSettings::default()
    });
    document.command_polling.enabled = true;
    document.command_polling.interval_seconds = 30;
    harness
        .operations
        .set_metrics_policy(&harness.subject, None, document)
        .unwrap();
    // Opt-in alone is insufficient: a stopped server must receive no commands.
    sampler.tick(&runtime, at + 1);
    assert!(runtime.game_snapshot().unwrap().generation.is_none());
    start(&harness, &owned.runtime);
    wait_for(|| {
        runtime.readiness().unwrap().readiness == crate::operations::readiness::Readiness::Ready
    });
    let started = Instant::now();
    let deadline = started + Duration::from_secs(8);
    loop {
        sampler.tick(&runtime, at + started.elapsed().as_millis() as i64 + 10);
        let current = harness
            .operations
            .metrics_current(&harness.subject)
            .unwrap();
        if current
            .sample
            .as_ref()
            .is_some_and(|s| s.values["tps"] == Some(19.5))
        {
            assert_eq!(
                current.sample.unwrap().tick_source.as_deref(),
                Some("command")
            );
            break;
        }
        assert!(
            Instant::now() < deadline,
            "correlated tick response must reach sampler"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    let commands = fs::read_to_string(harness.cwd.join("operation-commands")).unwrap();
    assert_eq!(commands.lines().filter(|line| *line == "tps").count(), 1);
    document.command_polling.enabled = false;
    harness
        .operations
        .set_metrics_policy(&harness.subject, Some(1), document)
        .unwrap();
    sampler.tick(&runtime, at + 10_000);
    assert_eq!(
        fs::read_to_string(harness.cwd.join("operation-commands")).unwrap(),
        commands
    );
    fs::write(harness.cwd.join("metrics-tick-no-response"), b"no response").unwrap();
    document.command_polling.enabled = true;
    harness
        .operations
        .set_metrics_policy(&harness.subject, Some(2), document)
        .unwrap();
    let retry_started = Instant::now();
    let deadline = retry_started + Duration::from_secs(8);
    loop {
        sampler.tick(
            &runtime,
            at + 20_000 + retry_started.elapsed().as_millis() as i64,
        );
        let current = harness
            .operations
            .metrics_current(&harness.subject)
            .unwrap();
        if current.sample.as_ref().is_some_and(|s| {
            s.tick_reason.as_deref() == Some("tick command timed out or output was unrecognized")
        }) {
            let sample = current.sample.unwrap();
            assert_eq!(sample.values["tps"], None);
            assert_eq!(sample.tick_source, None);
            break;
        }
        assert!(
            Instant::now() < deadline,
            "timed out tick request must become unknown"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    // Timeout backoff is 4x the interval, not an immediate retry on each runner tick.
    let commands = fs::read_to_string(harness.cwd.join("operation-commands")).unwrap();
    assert_eq!(commands.lines().filter(|line| *line == "tps").count(), 2);
    document.command_polling.enabled = false;
    harness
        .operations
        .set_metrics_policy(&harness.subject, Some(3), document)
        .unwrap();
    sampler.tick(&runtime, at + 30_000);
    fs::write(harness.cwd.join("release-game"), b"release").unwrap();
    wait_for(|| runtime.game_snapshot().unwrap().generation.is_none());
}
