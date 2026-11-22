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


#[test]
fn req_host_management_wire_grants_memberships_and_disables_one_server() {
    let host = create_host(Arc::new(FakeInstaller::default()));
    let owner = host.state.snapshot().unwrap().primary_account_id.unwrap();
    let target = host.state.device(host.devices.1).unwrap().unwrap().account_id;
    let version = || host.state.snapshot().unwrap().revision.to_string();
    let actor = Actor::Account(owner);
    let server = Uuid::new_v4();
    host.state.register_server(actor, &ServerEntry {
        server_id:server,name:"Test server".into(),game_port:free_port(),
        state:ServerState::Stopped,visibility:Visibility::Private,
    }, host.state.snapshot().unwrap().revision).unwrap();
    assert_eq!(host.call("GET", "/v1/host", Value::Null, &host.stranger).0, 403);
    let root = "/v1/host/manage";
    let (status, grants) = host.call("PUT", &format!("{root}/people/{target}/capabilities"),
        json!({"expected_version":version(),"capabilities":["host.settings.edit"]}), &host.owner);
    assert_eq!(status, 200, "{grants}");
    let (status, settings) = host.call("PATCH", "/v1/host", json!({"name":"Updated"}), &host.stranger);
    assert_eq!(status, 200, "{settings}");
    let (_, info) = host.call("GET", "/v1/host", Value::Null, &host.stranger);
    assert_eq!(info["hostCapabilities"], json!(["host.view","host.settings.edit"]));
    let (_, catalog) = host.call("GET", &format!("{root}/capabilities"), Value::Null, &host.stranger);
    let caps = catalog["data"]["capabilities"].as_array().unwrap();
    for (id, layer) in [("devices.revoke","host"),("lifecycle.enable","server")] {
        assert!(caps.iter().any(|value| value["id"] == id && value["layer"] == layer));
    }
    let path = format!("{root}/people/{target}/memberships/{server}");
    assert_eq!(host.call("PUT", &path, json!({"expected_version":version()}), &host.owner).0, 400);
    let (status, _) = host.call("PUT", &path,
        json!({"expected_version":version(),"role_id":"viewer"}), &host.owner);
    assert_eq!(status, 200);
    let path = format!("/v1/servers/{server}/manage/enabled");
    let patch = || json!({"expected_version":version(),"enabled":false});
    assert_eq!(host.call("PATCH", &path, patch(), &host.stranger).0, 403);
    let (status, reply) = host.call("PATCH", &path, patch(), &host.owner);
    assert_eq!(status, 200, "{reply}");
    let (_, cards) = host.call("GET", "/v1/host/servers", Value::Null, &host.stranger);
    assert_eq!(cards["servers"][0]["state"], "disabled");
    assert_eq!(cards["servers"][0]["enabled"], false);
    let (status, view) = host.call("GET", &path, Value::Null, &host.stranger);
    assert_eq!(status, 200, "{view}");
    assert_eq!(view["data"]["can_manage"], false);
    let (status, reply) = host.call("GET", &format!("/v1/servers/{server}/manage/server"), Value::Null, &host.stranger);
    assert_eq!(status, 409);
    assert_eq!(reply["error"]["code"], "server_disabled");
}
