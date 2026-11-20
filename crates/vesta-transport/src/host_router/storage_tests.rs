use super::*;

#[test]
fn host_storage_is_owner_only_and_reads_background_measurements() {
    let host = create_host(Arc::new(FakeInstaller::default()));
    let owner = host.state.snapshot().unwrap().primary_account_id.unwrap();
    let directories = host.router.imports().unwrap().servers_root();
    let id = Uuid::new_v4();
    host.state
        .register_server(
            Actor::Account(owner),
            &ServerEntry {
                server_id: id,
                name: "Measured".into(),
                game_port: free_port(),
                state: ServerState::Stopped,
                visibility: Visibility::Private,
            },
            host.state.snapshot().unwrap().revision,
        )
        .unwrap();
    let dir = directories.join(id.to_string());
    for (path, size) in [
        ("game/world/level.dat", 14),
        ("game/logs/latest.log", 9),
        ("game/mods/example.jar", 15),
        ("game/server.jar", 22),
        ("backups/repository/backup", 5),
        ("private/marker", 3),
    ] {
        let path = dir.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, vec![1; size]).unwrap();
    }
    let path = "/v1/host/storage";
    assert_eq!(host.call("GET", path, Value::Null, &host.stranger).0, 403);
    assert_eq!(
        host.call(
            "POST",
            &format!("{path}/measure"),
            json!({}),
            &host.stranger
        )
        .0,
        403
    );
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    let snapshot = loop {
        let (status, snapshot) = host.call("GET", path, Value::Null, &host.owner);
        assert_eq!(status, 200, "{snapshot}");
        if snapshot["servers"]
            .as_array()
            .unwrap()
            .iter()
            .any(|server| server["bytes"] == 68)
        {
            break snapshot;
        }
        assert!(std::time::Instant::now() < deadline, "{snapshot}");
        std::thread::sleep(Duration::from_millis(400));
    };
    assert!(snapshot["measuredAt"].is_string());
    assert_eq!(snapshot["backups"]["bytes"], 5);
    assert_eq!(
        snapshot["servers"][0]["parts"],
        json!({
            "worlds":14,"content":15,"backups":5,"logs":9,"software":22,"trash":0,"other":3
        })
    );
    let volume = &snapshot["volumes"][0];
    let free = volume["freeBytes"].as_u64().unwrap();
    let total = volume["totalBytes"].as_u64().unwrap();
    assert!(total > 0 && free <= total);
    // A write to disk after publishing the snapshot is visible on the next
    // measurement. The request itself never walks the filesystem.
    std::fs::write(dir.join("game/world/level.dat"), vec![1; 141]).unwrap();
    assert_eq!(
        host.call("GET", path, Value::Null, &host.owner).1["servers"][0]["bytes"],
        68
    );
    assert!(matches!(
        host.call("POST", &format!("{path}/measure"), json!({}), &host.owner)
            .0,
        202 | 429
    ));
}
