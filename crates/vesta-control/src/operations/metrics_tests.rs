//! Reuses the owned inert child; metrics must never write `list` or any command.
use super::*;
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
