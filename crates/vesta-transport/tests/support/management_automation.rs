//! Real loopback TLS coverage for workflow automation and the principal inbox.
use super::*;
use vesta_control::identity::BuiltinRole;

#[test]
fn backup_setup_works_without_host_configuration_and_requires_reserved_authority() {
    let f = Fixture::new("minecraft", 8, 16384, 5);
    let (_, _, owner, _) = actor(&f, BuiltinRole::Owner);
    let (_, _, admin, _) = actor(&f, BuiltinRole::Administrator);
    let (_, _, viewer, _) = actor(&f, BuiltinRole::Viewer);
    let initial = send(&f, "GET", "backups/config", b"", Some(&owner), None);
    assert_eq!(initial.status, 200);
    assert_eq!(initial.json()["data"]["profiles"], serde_json::json!([]));
    assert_eq!(initial.json()["data"]["repository"]["initialized"], false);
    assert_eq!(
        send(&f, "GET", "backups/config", b"", Some(&viewer), None).status,
        403
    );
    assert_eq!(
        send(&f, "GET", "backups/config", b"", Some(&admin), None).status,
        200
    );
    let doc = serde_json::json!({"expected_revision":null,"initialize_repository":true,"profiles":[{"name":"daily","mode":"stopped","keep_last":7}]});
    let key = "backup-setup-intent-0001";
    assert_eq!(
        call(&f, "PUT", "backups/config", &doc, &viewer, key).status,
        403
    );
    let saved = call(&f, "PUT", "backups/config", &doc, &admin, key);
    assert_eq!(saved.status, 200, "{}", saved.json());
    assert_eq!(saved.json()["data"]["revision"], 1);
    assert_eq!(
        call(&f, "PUT", "backups/config", &doc, &admin, key).json()["data"],
        saved.json()["data"]
    );
    assert_eq!(
        call(
            &f,
            "PUT",
            "backups/config",
            &doc,
            &owner,
            "backup-stale-intent-001"
        )
        .status,
        409
    );
    let mut path = doc.clone();
    path["repository"] = serde_json::json!("C:/arbitrary");
    assert_eq!(
        call(
            &f,
            "PUT",
            "backups/config",
            &path,
            &owner,
            "backup-path-intent-0001"
        )
        .status,
        400
    );
    assert_eq!(
        send(&f, "GET", "server", b"", Some(&owner), None).json()["data"]["backups_configured"],
        true
    );
    let empty = serde_json::json!({"expected_revision":1,"profiles":[]});
    assert_eq!(
        call(
            &f,
            "PUT",
            "backups/config",
            &empty,
            &owner,
            "backup-delete-intent-01"
        )
        .status,
        200
    );
    assert_eq!(
        send(&f, "GET", "server", b"", Some(&owner), None).json()["data"]["backups_configured"],
        false
    );
    assert_eq!(
        send(&f, "GET", "backups/config", b"", Some(&owner), None).json()["data"]["repository"]["initialized"],
        true
    );
}

fn send(
    f: &Fixture,
    method: &str,
    path: &str,
    body: &[u8],
    token: Option<&str>,
    key: Option<&str>,
) -> WireReply {
    let mut stream = f.stream();
    write!(stream, "{method} /api/v3/manage/{path} HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n", body.len()).unwrap();
    if let Some(token) = token {
        write!(stream, "Authorization: Bearer {token}\r\n").unwrap();
    }
    if let Some(key) = key {
        write!(stream, "Idempotency-Key: {key}\r\n").unwrap();
    }
    stream.write_all(b"\r\n").unwrap();
    stream.write_all(body).unwrap();
    stream.flush().unwrap();
    read_reply(stream).unwrap()
}

fn actor(
    f: &Fixture,
    role: BuiltinRole,
) -> (String, String, String, ring::signature::Ed25519KeyPair) {
    let (device, key) = enroll_management(f, role);
    let token = management_session(f, &device, &key);
    let principal = f
        .host()
        .database()
        .read(|c| {
            c.query_row(
                "SELECT principal_id FROM devices WHERE id=?",
                [&device],
                |r| r.get::<_, String>(0),
            )
            .map_err(Into::into)
        })
        .unwrap();
    (principal, device, token, key)
}

fn call(
    f: &Fixture,
    method: &str,
    path: &str,
    body: &serde_json::Value,
    token: &str,
    key: &str,
) -> WireReply {
    send(
        f,
        method,
        path,
        &serde_json::to_vec(body).unwrap(),
        Some(token),
        Some(key),
    )
}

fn definition(action: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "trigger":{"kind":"manual"},
        "steps":[{"id":"step-one","action":action,"timeout_secs":60}]
    })
}

#[test]
fn workflow_tls_owner_crud_manual_run_and_idempotent_replay() {
    let f = Fixture::new("minecraft", 8, 16384, 5);
    let (principal, _, owner, _) = actor(&f, BuiltinRole::Owner);
    let create = serde_json::json!({"name":"TLS workflow","execution_principal_id":principal,
        "definition":definition(serde_json::json!({"kind":"delay","secs":1})),"enabled":true});
    let key = "tls-workflow-create-0001";
    let created = call(&f, "POST", "automation/workflows", &create, &owner, key);
    assert_eq!(created.status, 201);
    let outcome = &created.json()["data"];
    assert_eq!(outcome["kind"], "workflow");
    assert_eq!(outcome["revision"], 1);
    let id = outcome["workflow_id"].as_str().unwrap().to_owned();
    assert!(id.len() <= 64);
    let replay = call(&f, "POST", "automation/workflows", &create, &owner, key);
    assert_eq!(replay.status, 201);
    assert_eq!(replay.json()["data"], created.json()["data"]);
    let changed = serde_json::json!({"name":"Different intent","execution_principal_id":principal,
        "definition":definition(serde_json::json!({"kind":"delay","secs":1})),"enabled":true});
    let conflict = call(&f, "POST", "automation/workflows", &changed, &owner, key);
    assert_eq!(conflict.status, 409);
    assert_eq!(conflict.json()["error"]["code"], "idempotency_conflict");

    let list = send(
        &f,
        "GET",
        "automation/workflows?limit=10",
        b"",
        Some(&owner),
        None,
    );
    assert_eq!(list.status, 200);
    let summary = &list.json()["data"]["workflows"][0];
    assert_eq!(summary["trigger"]["kind"], "manual");
    assert_eq!(summary["id"], id);
    assert!(
        summary.get("definition").is_none(),
        "list must omit definitions"
    );
    let detail = send(
        &f,
        "GET",
        &format!("automation/workflows/{id}"),
        b"",
        Some(&owner),
        None,
    );
    assert_eq!(detail.status, 200);
    assert_eq!(
        detail.json()["data"]["workflow"]["definition"]["steps"][0]["id"],
        "step-one"
    );

    let update = serde_json::json!({"expected_revision":1,"name":"TLS workflow updated","execution_principal_id":principal,
        "definition":definition(serde_json::json!({"kind":"notify","severity":"info","title":"Hello","body":"from TLS"}))});
    let updated = call(
        &f,
        "PUT",
        &format!("automation/workflows/{id}"),
        &update,
        &owner,
        "tls-workflow-update-0001",
    );
    assert_eq!(updated.status, 200);
    assert_eq!(updated.json()["data"]["kind"], "workflow");
    assert_eq!(updated.json()["data"]["revision"], 2);
    let disabled = call(
        &f,
        "POST",
        &format!("automation/workflows/{id}/enabled"),
        &serde_json::json!({"expected_revision":2,"enabled":false}),
        &owner,
        "tls-workflow-enabled-0001",
    );
    assert_eq!(disabled.status, 200);
    assert_eq!(disabled.json()["data"]["kind"], "workflow");
    let enabled = call(
        &f,
        "POST",
        &format!("automation/workflows/{id}/enabled"),
        &serde_json::json!({"expected_revision":3,"enabled":true}),
        &owner,
        "tls-workflow-enabled-0002",
    );
    assert_eq!(enabled.status, 200);
    assert_eq!(enabled.json()["data"]["kind"], "workflow");

    let manual = call(
        &f,
        "POST",
        &format!("automation/workflows/{id}/run"),
        &serde_json::json!({}),
        &owner,
        "tls-workflow-manual-0001",
    );
    assert_eq!(manual.status, 202);
    assert_eq!(manual.json()["data"]["kind"], "run");
    assert_eq!(manual.json()["data"]["workflow_id"], id);
    let run_id = manual.json()["data"]["run_id"].as_str().unwrap().to_owned();
    assert!(run_id.len() <= 64);
    let runs = send(
        &f,
        "GET",
        &format!("automation/workflows/{id}/runs?limit=10"),
        b"",
        Some(&owner),
        None,
    );
    assert_eq!(runs.status, 200);
    assert_eq!(runs.json()["data"]["runs"][0]["id"], run_id);
    assert!(runs.json()["data"]["runs"][0].get("definition").is_none());
    let run = send(
        &f,
        "GET",
        &format!("automation/runs/{run_id}"),
        b"",
        Some(&owner),
        None,
    );
    assert_eq!(run.status, 200);
    assert!(run.json()["data"]["run"]["definition"].is_object());
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let steps = send(
            &f,
            "GET",
            &format!("automation/runs/{run_id}/steps?limit=10"),
            b"",
            Some(&owner),
            None,
        );
        assert_eq!(steps.status, 200);
        let steps_json = steps.json();
        let values = steps_json["data"]["steps"].as_array().unwrap();
        if !values.is_empty() {
            assert_eq!(values[0]["run_id"], run_id);
            assert_eq!(values[0]["index"], 0);
            assert_eq!(values[0]["state"], "succeeded");
            break;
        }
        assert!(
            Instant::now() < deadline,
            "automation step did not become inspectable"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
    let delete_body = serde_json::json!({"expected_revision":4});
    let delete_key = "tls-workflow-delete-0001";
    let deleted = call(
        &f,
        "DELETE",
        &format!("automation/workflows/{id}"),
        &delete_body,
        &owner,
        delete_key,
    );
    assert_eq!(deleted.status, 200);
    assert_eq!(deleted.json()["data"]["kind"], "deleted");
    assert_eq!(deleted.json()["data"]["workflow_id"], id);
    let replay = call(
        &f,
        "DELETE",
        &format!("automation/workflows/{id}"),
        &delete_body,
        &owner,
        delete_key,
    );
    assert_eq!(replay.status, 200);
    assert_eq!(replay.json()["data"], deleted.json()["data"]);
    let history = send(
        &f,
        "GET",
        "automation/runs?limit=1",
        b"",
        Some(&owner),
        None,
    );
    assert_eq!(history.status, 200);
    let history = history.json();
    assert_eq!(history["data"]["runs"][0]["id"], run_id);
    assert!(history["data"]["runs"][0].get("definition").is_none());
    assert_eq!(history["data"]["next_after"], run_id);
    let next = send(
        &f,
        "GET",
        &format!("automation/runs?limit=1&after={run_id}"),
        b"",
        Some(&owner),
        None,
    );
    assert!(next.json()["data"]["runs"].as_array().unwrap().is_empty());
    assert_eq!(
        send(
            &f,
            "GET",
            "automation/runs?limit=0",
            b"",
            Some(&owner),
            None
        )
        .status,
        400
    );
    let (_, _, viewer, _) = actor(&f, BuiltinRole::Viewer);
    assert_eq!(
        send(&f, "GET", "automation/runs", b"", Some(&viewer), None).status,
        403
    );
}

#[test]
fn inbox_tls_moderator_scope_shared_principal_and_viewer_refusal() {
    let f = Fixture::new("minecraft", 8, 16384, 5);
    let (principal, _, moderator, _) = actor(&f, BuiltinRole::Moderator);
    let (_, device, _, shared_key) = actor(&f, BuiltinRole::Moderator);
    // A second authenticated device is attached to the first principal; inbox state
    // is principal-scoped, while both HTTP credentials remain real signed sessions.
    let changed = f.host().database().write(|c| {
        c.execute(
            "UPDATE devices SET principal_id=? WHERE id=?",
            rusqlite::params![principal, device],
        )?;
        Ok(())
    });
    changed.unwrap();
    let shared_device_token = management_session(&f, &device, &shared_key);
    let notice = f
        .host()
        .automation()
        .notify(
            vesta_control::automation::Severity::Warning,
            "TLS inbox",
            "shared read state",
            "fixture",
        )
        .unwrap();
    let page = send(
        &f,
        "GET",
        "notifications?cursor=0&limit=10",
        b"",
        Some(&moderator),
        None,
    );
    assert_eq!(page.status, 200);
    let item = &page.json()["data"]["notifications"][0];
    assert_eq!(item["id"], notice);
    assert_eq!(item["read"], false);
    let ack = call(
        &f,
        "POST",
        "notifications/acknowledge",
        &serde_json::json!({"ids":[notice]}),
        &moderator,
        "tls-inbox-acknowledge-0001",
    );
    assert_eq!(ack.status, 200);
    assert_eq!(ack.json()["data"]["kind"], "notifications_acknowledged");
    let shared = send(
        &f,
        "GET",
        "notifications?cursor=0&limit=10",
        b"",
        Some(&shared_device_token),
        None,
    );
    assert_eq!(shared.status, 200);
    assert_eq!(
        shared.json()["data"]["notifications"][0]["acknowledged"],
        true
    );
    let read = call(
        &f,
        "POST",
        "notifications/read",
        &serde_json::json!({"ids":[notice]}),
        &shared_device_token,
        "tls-inbox-read-0001",
    );
    assert_eq!(read.status, 200);
    assert_eq!(read.json()["data"]["kind"], "notifications_read");

    let (_, _, viewer, _) = actor(&f, BuiltinRole::Viewer);
    let denied = send(&f, "GET", "notifications/unread", b"", Some(&viewer), None);
    assert_eq!(denied.status, 403);
    assert_eq!(denied.json()["error"]["code"], "management_forbidden");
}

#[test]
fn automation_adapter_strict_wire_contract_and_disabled_backups() {
    let f = Fixture::new("minecraft", 8, 16384, 5);
    let (principal, _, owner, _) = actor(&f, BuiltinRole::Owner);
    let create = serde_json::json!({"name":"strict","execution_principal_id":principal,
        "definition":definition(serde_json::json!({"kind":"delay","secs":1})),"enabled":true});
    for (key, expected) in [("bad key with space", 400), ("x", 400)] {
        assert_eq!(
            call(&f, "POST", "automation/workflows", &create, &owner, key).status,
            expected
        );
    }
    let unknown = serde_json::json!({"name":"strict","execution_principal_id":principal,"enabled":true,"definition":definition(serde_json::json!({"kind":"start","shell":"no"}))});
    let invalid = call(
        &f,
        "POST",
        "automation/workflows",
        &unknown,
        &owner,
        "tls-workflow-invalid-0001",
    );
    assert_eq!(invalid.status, 400);
    assert_eq!(invalid.json()["error"]["code"], "invalid_request");
    for query in ["?extra=1", "?limit=1&limit=2", "?after=bad%2Fid"] {
        let response = send(
            &f,
            "GET",
            &format!("automation/workflows{query}"),
            b"",
            Some(&owner),
            None,
        );
        assert_eq!(response.status, 400, "query {query}");
    }
    let backups = send(&f, "GET", "backups", b"", Some(&owner), None);
    assert_eq!(backups.status, 404);
    assert_eq!(backups.json()["error"]["code"], "backups_not_configured");
    let submit = call(
        &f,
        "POST",
        "backups/run",
        &serde_json::json!({"profile":"daily"}),
        &owner,
        "tls-backup-disabled-0001",
    );
    assert_eq!(submit.status, 404);
    assert_eq!(submit.json()["error"]["code"], "backups_not_configured");
}

#[test]
fn automation_mutations_require_one_json_content_type_and_valid_idempotency_key() {
    let f = Fixture::new("minecraft", 8, 16384, 5);
    let (principal, _, token, _) = actor(&f, BuiltinRole::Owner);
    let body = serde_json::to_vec(&serde_json::json!({"name":"strict","execution_principal_id":principal,"definition":definition(serde_json::json!({"kind":"delay","secs":1})),"enabled":true})).unwrap();
    for headers in [
        "Content-Type: text/plain\r\nIdempotency-Key: tls-content-type-0001\r\n",
        "Content-Type: application/json\r\nContent-Type: application/json\r\nIdempotency-Key: tls-duplicate-type-001\r\n",
        "Content-Type: application/json\r\n",
    ] {
        let mut stream = f.stream();
        write!(stream, "POST /api/v3/manage/automation/workflows HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\nAuthorization: Bearer {token}\r\n{headers}\r\n", body.len()).unwrap();
        stream.write_all(&body).unwrap();
        stream.flush().unwrap();
        assert_eq!(read_reply(stream).unwrap().status, 400);
    }
}

#[test]
fn workflow_tls_cached_mutation_refuses_current_capability_revocation() {
    let f = Fixture::new("minecraft", 8, 16384, 5);
    let (principal, _, token, _) = actor(&f, BuiltinRole::Administrator);
    let create = serde_json::json!({"name":"revocation","execution_principal_id":principal,
        "definition":definition(serde_json::json!({"kind":"delay","secs":1})),"enabled":true});
    let key = "tls-revoked-replay-key-0001";
    assert_eq!(
        call(&f, "POST", "automation/workflows", &create, &token, key).status,
        201
    );
    f.host()
        .database()
        .write(|c| {
            c.execute(
                "UPDATE role_assignments SET role_id='viewer' WHERE principal_id=?",
                [&principal],
            )?;
            Ok(())
        })
        .unwrap();
    let reply = call(&f, "POST", "automation/workflows", &create, &token, key);
    assert_eq!(reply.status, 403);
    assert_eq!(reply.json()["error"]["code"], "management_forbidden");
    let list = send(&f, "GET", "automation/workflows", b"", Some(&token), None);
    assert_eq!(list.status, 403);
    let anonymous = send(&f, "GET", "notifications", b"", None, None);
    assert_eq!(anonymous.status, 401);
}

#[test]
fn backup_tls_composite_cursor_and_literal_restore_validation_precede_service_lookup() {
    let f = Fixture::new("minecraft", 8, 16384, 5);
    let (_, _, owner, _) = actor(&f, BuiltinRole::Owner);
    for query in [
        "before=1234",
        "before=b1.8000000000000000.61",
        "before=b1.0000000000000000.2f",
        "limit=201",
        "limit=1&limit=2",
    ] {
        let reply = send(
            &f,
            "GET",
            &format!("backups?{query}"),
            b"",
            Some(&owner),
            None,
        );
        assert_eq!(reply.status, 400, "{query}");
        assert_eq!(reply.json()["error"]["code"], "invalid_request");
    }
    let valid = send(
        &f,
        "GET",
        "backups?before=b1.0000000000000000.6a6f622d61&limit=1",
        b"",
        Some(&owner),
        None,
    );
    assert_eq!(valid.status, 404);
    assert_eq!(valid.json()["error"]["code"], "backups_not_configured");
    for (n, path) in [
        "world/*",
        "world/file?",
        "world/[file]",
        "world/{file}",
        "../world",
    ]
    .iter()
    .enumerate()
    {
        let reply = call(
            &f,
            "POST",
            "backups/restore",
            &serde_json::json!({"profile":"daily","snapshot":"12345678","paths":[path]}),
            &owner,
            &format!("restore-pattern-rejection-{n:04}"),
        );
        assert_eq!(reply.status, 400, "{path}");
        assert_eq!(reply.json()["error"]["code"], "invalid_request");
    }
}

#[test]
fn snapshot_list_and_detail_strict_wire_queries() {
    let f = Fixture::new("minecraft", 8, 16384, 5);
    let (_, _, owner, _) = actor(&f, BuiltinRole::Owner);
    for path in [
        "backups/snapshots?profile=daily&limit=1",
        "backups/safety-snapshots?profile=daily&limit=100",
        "backups/snapshots/0123456789abcdef0123456789abcdef?profile=daily",
    ] {
        let reply = send(&f, "GET", path, b"", Some(&owner), None);
        assert_eq!(reply.status, 404);
        assert_eq!(reply.json()["error"]["code"], "backups_not_configured");
    }
    for path in [
        "backups/snapshots?profile=daily&limit=101",
        "backups/snapshots?profile=daily&before=bogus",
        "backups/snapshots/0123456789abcdef0123456789abcdef?profile=daily&limit=1",
        "backups/snapshots/not-an-id?profile=daily",
    ] {
        let reply = send(&f, "GET", path, b"", Some(&owner), None);
        assert_eq!(reply.status, 400);
    }
    assert_eq!(
        send(
            &f,
            "GET",
            "backups/snapshots/0123456789abcdef0123456789abcdef?profile=daily",
            b"",
            None,
            None
        )
        .status,
        401
    );
}
