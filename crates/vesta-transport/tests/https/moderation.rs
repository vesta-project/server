use super::*;

#[test]
fn startup_whitelist_api_projects_and_cancels_without_writing_file() {
    use vesta_control::identity::BuiltinRole;
    let f = Fixture::inert();
    let (device, key) = enroll_management(&f, BuiltinRole::Owner);
    let owner = management_session(&f, &device, &key);
    let game = f.host().configuration().game().to_path_buf();
    std::fs::write(game.join("server.properties"), b"online-mode=false\n").unwrap();
    let g = management_start_inert(&f, &owner);
    let patch = serde_json::json!({"player":"alex","add":true,"expected_generation":g});
    let accepted = management_mutation(
        &f,
        &owner,
        "players/whitelist/startup",
        "startup-http-add-001",
        &patch,
    );
    assert_eq!(accepted.status, 200, "{}", accepted.json());
    let id = accepted.json()["data"]["intent_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let list = f.request("GET", "/api/v3/manage/players/whitelist", b"", Some(&owner));
    assert_eq!(list.json()["data"]["items"][0]["name"], "alex");
    assert!(!game.join("whitelist.json").exists());
    let remove = serde_json::json!({"player":"alex","add":false,"expected_generation":g});
    assert_eq!(
        management_mutation(
            &f,
            &owner,
            "players/whitelist/startup",
            "startup-http-remove-001",
            &remove
        )
        .status,
        200
    );
    let receipt = f.request(
        "GET",
        &format!("/api/v3/manage/players/whitelist/startup?ids={id}"),
        b"",
        Some(&owner),
    );
    assert_eq!(receipt.status, 200);
    assert_eq!(receipt.json()["data"]["items"][0]["status"], "cancelled");
    assert_eq!(
        management_mutation(
            &f,
            &owner,
            "players/whitelist/startup",
            "startup-http-add-001",
            &patch
        )
        .json()["data"]["status"],
        "cancelled"
    );
    assert!(
        f.request("GET", "/api/v3/manage/players/whitelist", b"", Some(&owner))
            .json()["data"]["items"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        f.request(
            "GET",
            "/api/v3/manage/players/whitelist/startup?ids=bad",
            b"",
            Some(&owner)
        )
        .status,
        400
    );
    let unknown = "123e4567-e89b-42d3-a456-426614174000";
    let missing = f.request(
        "GET",
        &format!("/api/v3/manage/players/whitelist/startup?ids={unknown}"),
        b"",
        Some(&owner),
    );
    assert_eq!(missing.json()["data"]["items"][0]["status"], "unknown");
    let stop = management_mutation(
        &f,
        &owner,
        "lifecycle/stop",
        "startup-http-stop-001",
        &serde_json::json!({"expected_generation":g}),
    );
    management_wait_operation(
        &f,
        &owner,
        stop.json()["data"]["operation_id"].as_str().unwrap(),
    );
}

#[test]
fn offline_whitelist_edits_are_fenced_recoverable_and_idempotent() {
    use vesta_control::identity::BuiltinRole;
    let f = Fixture::inert();
    let (device, key) = enroll_management(&f, BuiltinRole::Owner);
    let owner = management_session(&f, &device, &key);
    let game = f.host().configuration().game().to_path_buf();
    std::fs::write(game.join("server.properties"), b"online-mode=false\n").unwrap();
    let route = "/api/v3/manage/players/whitelist/offline";
    assert_eq!(f.request("GET", route, b"", None).status, 401);
    let (viewer_device, viewer_key) = enroll_management(&f, BuiltinRole::Viewer);
    let viewer = management_session(&f, &viewer_device, &viewer_key);
    assert_eq!(
        management_mutation(
            &f,
            &viewer,
            "players/whitelist/offline",
            "offline-forbidden-0001",
            &serde_json::json!({"player":"Notch","add":false,"expected_sha256":null})
        )
        .status,
        403
    );
    let before = f.request("GET", route, b"", Some(&owner));
    assert_eq!(before.status, 200);
    let patch = serde_json::json!({"player":"Notch","add":true,"expected_sha256":before.json()["data"]["sha256"]});
    let saved = management_mutation(
        &f,
        &owner,
        "players/whitelist/offline",
        "offline-whitelist-0001",
        &patch,
    );
    assert_eq!(saved.status, 200, "{}", saved.json());
    let bytes = std::fs::read(game.join("whitelist.json")).unwrap();
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&bytes).unwrap()[0]["uuid"],
        "b50ad385-829d-3141-a216-7e7d7539ba7f"
    );
    let replay = management_mutation(
        &f,
        &owner,
        "players/whitelist/offline",
        "offline-whitelist-0001",
        &patch,
    );
    assert_eq!(replay.status, 200);
    assert_eq!(replay.json()["data"], saved.json()["data"]);
    assert_eq!(std::fs::read(game.join("whitelist.json")).unwrap(), bytes);
    let stale = management_mutation(
        &f,
        &owner,
        "players/whitelist/offline",
        "offline-whitelist-stale",
        &patch,
    );
    assert_eq!(stale.status, 412);
    let now = f.request("GET", route, b"", Some(&owner));
    let remove = serde_json::json!({"player":"Notch","add":false,"expected_sha256":now.json()["data"]["sha256"]});
    let generation = management_start_inert(&f, &owner);
    // A lost receipt can be recovered after startup without reapplying the edit.
    assert_eq!(
        management_mutation(
            &f,
            &owner,
            "players/whitelist/offline",
            "offline-whitelist-0001",
            &patch
        )
        .status,
        200
    );
    assert_eq!(
        management_mutation(
            &f,
            &owner,
            "players/whitelist/offline",
            "offline-whitelist-0001",
            &remove
        )
        .status,
        409
    );
    assert_eq!(
        management_mutation(
            &f,
            &owner,
            "players/whitelist/offline",
            "offline-whitelist-running",
            &remove
        )
        .status,
        412
    );
    assert_eq!(std::fs::read(game.join("whitelist.json")).unwrap(), bytes);
    let stop = management_mutation(
        &f,
        &owner,
        "lifecycle/stop",
        "offline-whitelist-stop",
        &serde_json::json!({"expected_generation":generation}),
    );
    management_wait_operation(
        &f,
        &owner,
        stop.json()["data"]["operation_id"].as_str().unwrap(),
    );
    let removed = management_mutation(
        &f,
        &owner,
        "players/whitelist/offline",
        "offline-whitelist-remove",
        &remove,
    );
    assert_eq!(removed.status, 200, "{}", removed.json());
    assert!(
        vesta_control::moderation::whitelist(&game)
            .unwrap()
            .is_empty()
    );
    std::fs::write(game.join("whitelist.json"), b"private malformed content").unwrap();
    assert_ne!(f.request("GET", route, b"", Some(&owner)).status, 200);
    assert_eq!(
        std::fs::read(game.join("whitelist.json")).unwrap(),
        b"private malformed content"
    );
}

#[test]
fn management_typed_moderation_routes_preserve_authority_and_bounded_projections() {
    use vesta_control::identity::BuiltinRole;
    let f = Fixture::inert();
    let (owner_device, owner_key) = enroll_management(&f, BuiltinRole::Owner);
    let owner = management_session(&f, &owner_device, &owner_key);
    let (device, key) = enroll_management(&f, BuiltinRole::Moderator);
    let moderator = management_session(&f, &device, &key);
    let stopped = serde_json::json!({"action":"kick","player":"Alex","expected_generation":"00112233445566778899aabbccddeeff"});
    management_reply_error(
        &management_mutation(
            &f,
            &moderator,
            "players/moderate",
            "typed-stopped-0001",
            &stopped,
        ),
        412,
        "precondition_failed",
    );
    let generation = management_start_inert(&f, &owner);
    let input = serde_json::json!({"action":"kick","player":"Alex","reason":"typed reason only in history","expected_generation":generation});
    let accepted = management_mutation(
        &f,
        &moderator,
        "players/moderate",
        "typed-kick-route-0001",
        &input,
    );
    assert_eq!(accepted.status, 202);
    let id = accepted.json()["data"]["operation_id"]
        .as_str()
        .unwrap()
        .to_owned();
    management_wait_operation(&f, &owner, &id);
    assert_eq!(
        management_mutation(
            &f,
            &moderator,
            "players/moderate",
            "typed-kick-route-0001",
            &input
        )
        .json()["data"]["operation_id"],
        id
    );
    for action in ["op", "deop"] {
        management_reply_error(
            &management_mutation(
                &f,
                &moderator,
                "players/moderate",
                "typed-forbidden-0001",
                &serde_json::json!({"action":action,"player":"Alex","expected_generation":generation}),
            ),
            403,
            "management_forbidden",
        );
    }
    management_reply_error(
        &management_mutation(
            &f,
            &moderator,
            "console/command",
            "typed-no-console-0001",
            &serde_json::json!({"line":"stop","expected_generation":generation}),
        ),
        403,
        "management_forbidden",
    );
    let history = f.request(
        "GET",
        "/api/v3/manage/players/moderation?limit=1",
        b"",
        Some(&moderator),
    );
    assert_eq!(history.status, 200);
    assert_eq!(history.json()["data"]["items"][0]["operation_id"], id);
    assert_eq!(history.json()["data"]["items"][0]["outcome"], "succeeded");
    let audit = f.request("GET", "/api/v3/manage/audit?limit=200", b"", Some(&owner));
    audit.hides("typed reason only in history");
    let game = f.host().configuration().game().to_path_buf();
    for endpoint in ["bans", "ops", "whitelist"] {
        let reply = f.request(
            "GET",
            &format!("/api/v3/manage/players/{endpoint}"),
            b"",
            Some(&moderator),
        );
        assert_eq!(reply.status, 200);
    }
    std::fs::write(game.join("ops.json"), br#"[{"name":"Alex","uuid":"123e4567-e89b-42d3-a456-426614174000","level":3,"bypassesPlayerLimit":true}]"#).unwrap();
    let ops = f.request("GET", "/api/v3/manage/players/ops", b"", Some(&moderator));
    assert_eq!(ops.json()["data"]["items"][0]["level"], 3);
    assert_eq!(ops.json()["data"]["items"][0]["bypassesPlayerLimit"], true);
    std::fs::write(
        game.join("banned-players.json"),
        b"private malformed content",
    )
    .unwrap();
    let malformed = f.request("GET", "/api/v3/manage/players/bans", b"", Some(&moderator));
    management_reply_error(&malformed, 503, "players_file_malformed");
    malformed.hides("private malformed content");
    let op = serde_json::json!({"action":"op","player":"Alex","expected_generation":generation});
    let awaiting = management_mutation(&f, &owner, "players/moderate", "typed-op-route-0001", &op);
    assert_eq!(awaiting.json()["data"]["state"], "awaiting_confirmation");
    let mut confirmed = op;
    confirmed["confirmation"] = awaiting.json()["data"]["confirmation_token"].clone();
    let admitted = management_mutation(
        &f,
        &owner,
        "players/moderate",
        "typed-op-route-0001",
        &confirmed,
    );
    assert_eq!(admitted.status, 202);
    management_wait_operation(
        &f,
        &owner,
        admitted.json()["data"]["operation_id"].as_str().unwrap(),
    );
    let stop = management_mutation(
        &f,
        &owner,
        "lifecycle/stop",
        "typed-route-stop-0001",
        &serde_json::json!({"expected_generation":generation}),
    );
    assert_eq!(stop.status, 202);
    management_wait_operation(
        &f,
        &owner,
        stop.json()["data"]["operation_id"].as_str().unwrap(),
    );
}
