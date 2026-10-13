use super::*;

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
