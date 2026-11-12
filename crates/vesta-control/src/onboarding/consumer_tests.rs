use super::*;
use sha2::Digest as _;
#[test]
fn installation_consumer_fence_requires_clean_shutdown_quiescent_state_and_no_unknown_jobs() {
    use crate::onboarding::{consumer::Consumer, provisioning::Request};
    let f = Fixture::at("private/state");
    let (_, _, token) = f.enroll(BuiltinRole::Owner);
    f.setup.claim(&token).unwrap();
    let source = tempfile::tempdir().unwrap();
    std::fs::write(source.path().join("server.jar"), b"inert").unwrap();
    let p = owned_provisioning(&f, source.path());
    let job = p
        .prepare(
            &token,
            Request::AdoptCopy {
                name: "Owned".into(),
                minecraft_version: "1.21.1".into(),
            },
        )
        .unwrap();
    let review = provision_state(&p, &token, &job.id, "review");
    p.apply(
        &token,
        crate::onboarding::provisioning::Apply {
            job_id: job.id.clone(),
            confirmation: review.confirmation.unwrap(),
            idempotency_key: "owned-fence".into(),
            acknowledge_warnings: true,
            acknowledge_legacy_checksum: false,
        },
    )
    .unwrap();
    provision_state(&p, &token, &job.id, "ready");
    p.publish_ready(|id,server,host,launch|{
        std::fs::write(f.root.path().join("host.toml"),host).unwrap();std::fs::write(f.root.path().join("private/launch.toml"),launch).unwrap();
        std::fs::write(f.root.path().join("private/consumer.json"),serde_json::to_vec(&serde_json::json!({"schema":1,"installation_id":INSTALLATION,"server_id":server,"job_id":id,"configuration":"host.toml","state":"private/state","launch_sha256":hex::encode(sha2::Sha256::digest(launch.as_bytes())),"host_sha256":hex::encode(sha2::Sha256::digest(host.as_bytes())),"installed_manifest_sha256":"1".repeat(64)})).unwrap()).unwrap();Ok(())
    }).unwrap();
    p.shutdown().unwrap();
    drop(p);
    let config = crate::config::HostConfiguration::load_file(
        &f.root.path().join("host.toml"),
        f.root.path(),
    )
    .unwrap();
    let consumer = Consumer::from_configuration(&config).unwrap().unwrap();
    assert!(
        consumer.clone().stopped().is_err(),
        "an open WAL is not immutable stopped state"
    );
    let Fixture { root, db, setup } = f;
    drop(setup);
    db.close().unwrap();
    drop(db);
    let observed = consumer.observe_readonly();
    assert!(
        observed.is_ok(),
        "readonly fixture observation: {observed:?}"
    );
    let held = consumer.clone().stopped().unwrap();
    let observed = held.observe().unwrap();
    assert_eq!(observed.installation_id, INSTALLATION);
    assert_eq!(observed.state_schema, vesta_state::CURRENT_SCHEMA_VERSION);
    assert!(consumer.acquire_lease().is_err());
    assert!(
        StateDirectory::new(root.path().join("private/state"), None)
            .unwrap()
            .acquire_runner_lock()
            .is_err()
    );
    drop(held);
    std::fs::write(
        root.path()
            .join(crate::onboarding::consumer::SESSION_PENDING),
        b"owned unclean marker",
    )
    .unwrap();
    assert!(matches!(
        consumer.clone().stopped(),
        Err("update_consumer_shutdown_unproven")
    ));
    std::fs::remove_file(
        root.path()
            .join(crate::onboarding::consumer::SESSION_PENDING),
    )
    .unwrap();
    let state = StateDirectory::new(root.path().join("private/state"), None).unwrap();
    let db = StateDatabase::open(state.clone(), Arc::new(SystemClock)).unwrap();
    // Fixture rows test the actual job-state predicate, not a provider install.
    db.write(|c|{c.execute_batch("CREATE TABLE hosted_pack_jobs(id TEXT PRIMARY KEY,body TEXT NOT NULL); INSERT INTO hosted_pack_jobs VALUES('owned','{\"state\":\"completed\"}');")?;Ok(())}).unwrap();
    db.close().unwrap();
    drop(db);
    drop(consumer.clone().stopped().unwrap());
    let db = StateDatabase::open(state, Arc::new(SystemClock)).unwrap();
    db.write(|c| {
        c.execute("UPDATE hosted_pack_jobs SET body='{}'", [])?;
        Ok(())
    })
    .unwrap();
    db.close().unwrap();
    drop(db);
    assert!(matches!(
        consumer.stopped(),
        Err("update_consumer_writes_unresolved")
    ));
    assert_eq!(
        std::fs::read(source.path().join("server.jar")).unwrap(),
        b"inert"
    );
}
