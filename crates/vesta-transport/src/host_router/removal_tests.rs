use super::*;

#[test]
fn removed_server_lifecycle_routes_preserve_ids_and_backup_choices() {
    let host = create_host(Arc::new(FakeInstaller::default()));
    let owner = host.state.snapshot().unwrap().primary_account_id.unwrap();
    let root = host.router.imports().unwrap().servers_root();
    let register = |name: &str, port| {
        let id = Uuid::new_v4();
        host.state
            .register_server(
                Actor::Account(owner),
                &ServerEntry {
                    server_id: id,
                    name: name.to_owned(),
                    game_port: port,
                    state: ServerState::Stopped,
                    visibility: Visibility::Private,
                },
                host.state.snapshot().unwrap().revision,
            )
            .unwrap();
        let dir = root.join(id.to_string());
        std::fs::create_dir_all(dir.join("game")).unwrap();
        std::fs::write(
            dir.join("game/server.properties"),
            format!("motd=Hello\nserver-port={port}\n"),
        )
        .unwrap();
        std::fs::create_dir_all(dir.join("backups/repository")).unwrap();
        std::fs::write(dir.join("backups/repository/marker"), b"backup").unwrap();
        id
    };
    let host_path = "/v1/host";
    let (status, initial) = host.call("GET", host_path, Value::Null, &host.owner);
    assert_eq!(status, 200);
    assert_eq!(initial["removedServerRetentionDays"], 7);
    for cap in [
        "removed-servers",
        "server-purge",
        "removed-server-retention",
    ] {
        assert!(
            initial["capabilities"]
                .as_array()
                .unwrap()
                .contains(&json!(cap))
        );
    }
    assert_eq!(
        host.call(
            "PATCH",
            host_path,
            json!({"removedServerRetentionDays": 0}),
            &host.owner
        )
        .0,
        400
    );
    assert_eq!(
        host.call(
            "PATCH",
            host_path,
            json!({"removedServerRetentionDays": 3}),
            &host.stranger
        )
        .0,
        403
    );
    let port = free_port();
    let id = register("Survival", port);
    let path = format!("/v1/host/servers/{id}");
    let removed_path = format!("/v1/host/servers/removed/{id}");
    let restore_path = format!("{removed_path}/restore");
    let list = "/v1/host/servers/removed";
    let soft = json!({"confirmName":"Survival","idempotencyKey":Uuid::new_v4()});
    let (status, removed) = host.call("DELETE", &path, soft, &host.owner);
    assert_eq!(status, 200, "{removed}");
    let (status, settings) = host.call(
        "PATCH",
        host_path,
        json!({"removedServerRetentionDays": 3}),
        &host.owner,
    );
    assert_eq!(status, 200, "{settings}");
    assert_eq!(settings["removedServerRetentionDays"], 3);
    assert_eq!(host.call("GET", list, Value::Null, &host.stranger).0, 403);
    let (status, page) = host.call("GET", list, Value::Null, &host.owner);
    assert_eq!(status, 200, "{page}");
    let row = &page["servers"][0];
    assert_eq!(row["serverId"], json!(id));
    assert_eq!(row["restoreUntil"], removed["restoreUntil"]);
    assert_eq!(row["state"], "restorable");
    assert!(page["nextCursor"].is_null());

    register("Survival", port);
    register("Survival (restored)", free_port());
    let keyed = |method, path, key, auth, body: Value| {
        let headers = format!(
            "Authorization: {auth}\r\nContent-Type: application/json\r\nIdempotency-Key: {key}\r\n"
        );
        let (status, bytes, _) =
            host.wire()
                .raw(method, path, &headers, &serde_json::to_vec(&body).unwrap());
        (status, serde_json::from_slice::<Value>(&bytes).unwrap())
    };
    let restore = Uuid::new_v4();
    assert_eq!(
        keyed("POST", &restore_path, restore, &host.stranger, json!({})).0,
        403
    );
    let (status, blocked) = keyed(
        "POST",
        &restore_path,
        Uuid::new_v4(),
        &host.owner,
        json!({"gamePort":port}),
    );
    assert_eq!(status, 409, "{blocked}");
    assert_eq!(blocked["error"]["code"], "game_port_in_use");
    let (status, restored) = keyed("POST", &restore_path, restore, &host.owner, json!({}));
    assert_eq!(status, 200, "{restored}");
    assert_eq!(restored["serverId"], json!(id));
    assert_eq!(restored["name"], "Survival (restored 2)");
    assert_ne!(restored["gamePort"], port);
    let dir = root.join(id.to_string());
    assert!(
        std::fs::read_to_string(dir.join("game/server.properties"))
            .unwrap()
            .ends_with(&format!("server-port={}\n", restored["gamePort"]))
    );
    assert_eq!(
        keyed("POST", &restore_path, restore, &host.owner, json!({})),
        (200, restored)
    );
    assert_eq!(
        keyed(
            "POST",
            &restore_path,
            restore,
            &host.owner,
            json!({"name":"New"})
        )
        .0,
        409
    );

    let permanent = json!({"confirmName":"Survival (restored 2)","idempotencyKey":Uuid::new_v4(),"permanently":true});
    let (status, accepted) = host.call("DELETE", &path, permanent.clone(), &host.owner);
    assert_eq!(status, 202, "{accepted}");
    assert_eq!(
        host.call("DELETE", &path, permanent, &host.owner),
        (202, accepted)
    );
    assert_eq!(
        host.call("GET", list, Value::Null, &host.owner).1["servers"],
        json!([])
    );

    let discard = register("Discard", free_port());
    assert_eq!(
        host.call(
            "DELETE",
            &format!("/v1/host/servers/{discard}"),
            json!({"confirmName":"Discard","idempotencyKey":Uuid::new_v4()}),
            &host.owner
        )
        .0,
        200
    );
    let purge_path = format!("/v1/host/servers/removed/{discard}");
    let purge = Uuid::new_v4();
    let body = json!({"confirmName":"Discard","keepBackups":false});
    let (status, receipt) = keyed("DELETE", &purge_path, purge, &host.owner, body.clone());
    assert_eq!(status, 202, "{receipt}");
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    loop {
        let (_, page) = host.call("GET", list, Value::Null, &host.owner);
        let retained = root.with_file_name("retained-backups");
        if page["servers"] == json!([]) && retained.join(id.to_string()).join("marker").is_file() {
            assert!(!retained.join(discard.to_string()).exists());
            break;
        }
        assert!(std::time::Instant::now() < deadline, "{page}");
        std::thread::sleep(Duration::from_millis(400));
    }
    assert_eq!(
        keyed("DELETE", &purge_path, purge, &host.owner, body),
        (202, receipt)
    );
    let (status, gone) = keyed(
        "POST",
        &format!("{purge_path}/restore"),
        Uuid::new_v4(),
        &host.owner,
        json!({}),
    );
    assert_eq!(status, 410, "{gone}");
    assert_eq!(gone["error"]["code"], "gone");
    assert_eq!(
        host.state.server(id).unwrap().unwrap().state,
        ServerState::Deleted
    );
}
