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
    let permanent = json!({
        "confirmName":"Measured","idempotencyKey":Uuid::new_v4(),"permanently":true
    });
    let server_path = format!("/v1/host/servers/{id}");
    let (status, receipt) = host.call("DELETE", &server_path, permanent.clone(), &host.owner);
    assert_eq!(status, 202, "{receipt}");
    assert_eq!(receipt["bytes"], 68);
    let deadline = std::time::Instant::now() + Duration::from_secs(25);
    loop {
        let (_, snapshot) = host.call("GET", path, Value::Null, &host.owner);
        if snapshot["servers"] == json!([]) && snapshot["backups"]["outsideServers"] == 5 {
            break;
        }
        assert!(std::time::Instant::now() < deadline, "{snapshot}");
        std::thread::sleep(Duration::from_millis(250));
    }
    assert_eq!(
        host.call("DELETE", &server_path, permanent, &host.owner),
        (202, receipt)
    );
    assert!(matches!(
        host.call("POST", &format!("{path}/measure"), json!({}), &host.owner)
            .0,
        202 | 429
    ));
}

#[test]
fn storage_cleanup_previews_then_queues_removed_servers_and_replays_receipt() {
    let host = create_host(Arc::new(FakeInstaller::default()));
    let owner = host.state.snapshot().unwrap().primary_account_id.unwrap();
    let root = host.router.imports().unwrap().servers_root();
    let server = Uuid::new_v4();
    host.state
        .register_server(
            Actor::Account(owner),
            &ServerEntry {
                server_id: server,
                name: "Cleanup".into(),
                game_port: free_port(),
                state: ServerState::Stopped,
                visibility: Visibility::Private,
            },
            host.state.snapshot().unwrap().revision,
        )
        .unwrap();
    let dir = root.join(server.to_string());
    std::fs::create_dir_all(dir.join("game/world")).unwrap();
    std::fs::write(dir.join("game/world/marker"), b"test").unwrap();
    std::fs::create_dir_all(dir.join("backups/repository")).unwrap();
    std::fs::write(dir.join("backups/repository/marker"), b"backup").unwrap();
    std::fs::create_dir_all(root.join(".staging")).unwrap();
    std::fs::write(root.join(".staging/owned"), b"skip").unwrap();
    assert_eq!(
        host.call(
            "DELETE",
            &format!("/v1/host/servers/{server}"),
            json!({
                "confirmName":"Cleanup","idempotencyKey":Uuid::new_v4()
            }),
            &host.owner
        )
        .0,
        200
    );
    let path = "/v1/host/storage/cleanup";
    let mut payload = json!({"dryRun":true,"targets":[
        {"kind":"removed","serverId":server,"confirmName":"Cleanup"},
        {"kind":"cache","cache":"temp"}
    ]});
    assert_eq!(
        host.call("POST", path, payload.clone(), &host.stranger).0,
        403
    );
    let (status, mut preview) = host.call("POST", path, payload.clone(), &host.owner);
    assert_eq!(status, 200, "{preview}");
    assert_eq!(preview["items"][1]["skipped"], "in_use");
    assert!(preview["bytes"].as_u64().is_some_and(|bytes| bytes >= 4));
    let removed = host
        .call("GET", "/v1/host/servers/removed", Value::Null, &host.owner)
        .1;
    assert_eq!(removed["servers"][0]["state"], "restorable");
    payload["dryRun"] = json!(false);
    assert_eq!(host.call("POST", path, payload.clone(), &host.owner).0, 400);
    let key = Uuid::new_v4();
    let call = |body: &Value| {
        let headers = format!(
            "Authorization: {}\r\nContent-Type: application/json\r\nIdempotency-Key: {key}\r\n",
            host.owner
        );
        let (status, bytes, _) =
            host.wire()
                .raw("POST", path, &headers, &serde_json::to_vec(body).unwrap());
        (status, serde_json::from_slice::<Value>(&bytes).unwrap())
    };
    preview["dryRun"] = json!(false);
    assert_eq!(call(&payload), (200, preview.clone()));
    assert_eq!(call(&payload), (200, preview));
    payload["targets"][0]["confirmName"] = json!("Wrong");
    assert_eq!(call(&payload).0, 409);
    assert!(root.join(".staging/owned").is_file());
    let backup = root
        .with_file_name("retained-backups")
        .join(server.to_string())
        .join("marker");
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    while !backup.is_file() {
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(250));
    }
}
