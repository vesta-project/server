//! Owned TLS game/state fixtures; no provider network or Minecraft process.
use super::*;
fn send(
    f: &Fixture,
    method: &str,
    path: &str,
    body: &[u8],
    token: Option<&str>,
    key: Option<&str>,
    condition: Option<(&str, &str)>,
) -> WireReply {
    let mut stream = f.stream();
    write!(stream,"{method} {path} HTTP/1.1\r\nHost: localhost\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n",if method=="PUT" { "application/octet-stream" } else { "application/json" },body.len()).unwrap();
    if let Some(token) = token {
        write!(stream, "Authorization: Bearer {token}\r\n").unwrap();
    }
    if let Some(key) = key {
        write!(stream, "Idempotency-Key: {key}\r\n").unwrap();
    }
    if let Some((name, value)) = condition {
        write!(stream, "{name}: {value}\r\n").unwrap();
    }
    stream.write_all(b"\r\n").unwrap();
    stream.write_all(body).unwrap();
    stream.flush().unwrap();
    read_reply(stream).unwrap()
}
fn digest(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(bytes))
}
#[test]
fn server_content_inventory_mutation_and_family_refusals_are_real_routes() {
    let f = Fixture::new("minecraft", 8, 65536, 5);
    std::fs::create_dir_all(f.temp.path().join("game/mods")).unwrap();
    std::fs::write(f.temp.path().join("game/mods/unknown.jar"), b"unknown jar").unwrap();
    let (device, key) = enroll_management(&f, vesta_control::identity::BuiltinRole::Owner);
    let token = management_session(&f, &device, &key);
    let inventory = send(
        &f,
        "GET",
        "/api/v3/manage/content",
        b"",
        Some(&token),
        None,
        None,
    );
    assert_eq!(inventory.status, 200);
    assert_eq!(
        inventory.json()["data"]["items"][0]["filename"],
        "unknown.jar"
    );
    assert!(inventory.json()["data"]["items"][0]["provider"].is_null());
    let subject = f.host().management().authenticate(&token).unwrap();
    f.host().database().write(|c|{c.execute("INSERT INTO principal_overrides (id,principal_id,capability,scope,effect,granted_by,created_at) VALUES ('00000000-0000-4000-8000-000000000123',?,'config.view','*','deny','fixture',?)",rusqlite::params![subject.principal.as_str(),f.host().database().clock().millis()])?;Ok(())}).unwrap();
    for (kind, code) in [
        ("mod", "content_mods_unsupported"),
        ("plugin", "content_plugins_unsupported"),
    ] {
        let body=serde_json::json!({"kind":kind,"provider":"modrinth","project_id":"p","version_id":"v"}).to_string();
        management_reply_error(
            &send(
                &f,
                "POST",
                "/api/v3/manage/content/install",
                body.as_bytes(),
                Some(&token),
                Some("content-install-refused"),
                None,
            ),
            409,
            code,
        );
    }
    let body=serde_json::json!({"action":"disable","kind":"mod","path":"mods/unknown.jar","expected_sha256":digest(b"unknown jar")}).to_string();
    let changed = send(
        &f,
        "POST",
        "/api/v3/manage/content/mutate",
        body.as_bytes(),
        Some(&token),
        Some("content-disable-unknown"),
        None,
    );
    assert_eq!(changed.status, 200);
    assert_eq!(changed.json()["data"]["state"], "applied");
    assert!(
        f.temp
            .path()
            .join("game/mods/unknown.jar.disabled")
            .exists()
    );
}
#[test]
fn file_surfaces_refuse_pack_only_and_anonymous_devices() {
    let f = Fixture::new("minecraft", 8, 4096, 3);
    let mut sources = f.host().management().access_local().unwrap();
    sources.new_user = "player".into();
    f.host().management().set_access_local(sources).unwrap();
    let token = f.device_session();
    for (method, path) in [
        ("GET", "files"),
        ("GET", "files/content"),
        ("PUT", "files/content"),
        ("DELETE", "files"),
        ("POST", "files/uploads"),
        ("PUT", "files/uploads/00000000-0000-4000-8000-000000000000"),
        (
            "POST",
            "files/uploads/00000000-0000-4000-8000-000000000000/finish",
        ),
        (
            "DELETE",
            "files/uploads/00000000-0000-4000-8000-000000000000",
        ),
        ("GET", "trash"),
        ("POST", "trash/00000000-0000-4000-8000-000000000000/restore"),
        ("POST", "files/mkdir"),
        ("POST", "files/rename"),
        ("POST", "content/install"),
        ("GET", "content"),
        ("POST", "content/mutate"),
        ("POST", "content/update"),
        ("POST", "content/cancel"),
        ("POST", "content/identify"),
        ("GET", "config/server-properties"),
        ("PATCH", "config/server-properties"),
    ] {
        let body: &[u8] = if matches!(method, "POST" | "PATCH") {
            b"{}"
        } else {
            b""
        };
        let path = format!("/api/v3/manage/{path}");
        management_reply_error(
            &send(&f, method, &path, body, None, None, None),
            401,
            "management_session_invalid",
        );
        management_reply_error(
            &send(&f, method, &path, body, Some(&token), None, None),
            403,
            "management_forbidden",
        );
    }
}
#[test]
fn files_create_read_conflict_rename_trash_restore_and_audit() {
    let f = Fixture::new("minecraft", 8, 1_048_576, 5);
    let (device, key) = enroll_management(&f, vesta_control::identity::BuiltinRole::Owner);
    let token = management_session(&f, &device, &key);
    let send =
        |method: &str, path: &str, body: &[u8], key: &str, condition: Option<(&str, &str)>| {
            send(&f, method, path, body, Some(&token), Some(key), condition)
        };
    let path = "/api/v3/manage/files/content?path=config/a.txt";
    let created = send(
        "PUT",
        path,
        b"one",
        "files-create-key-0001",
        Some(("If-None-Match", "*")),
    );
    assert_eq!(created.status, 200);
    assert_eq!(created.json()["data"]["sha256"], digest(b"one"));
    assert_eq!(
        created.json()["data"],
        send(
            "PUT",
            path,
            b"one",
            "files-create-key-0001",
            Some(("If-None-Match", "*"))
        )
        .json()["data"]
    );
    management_reply_error(
        &send(
            "PUT",
            path,
            b"two",
            "files-create-key-0001",
            Some(("If-None-Match", "*")),
        ),
        409,
        "idempotency_conflict",
    );
    let existing = send(
        "PUT",
        path,
        b"one",
        "files-create-key-0002",
        Some(("If-None-Match", "*")),
    );
    management_reply_error(&existing, 412, "precondition_failed");
    assert_eq!(existing.json()["error"]["current_sha256"], digest(b"one"));
    let bad_hash = "0".repeat(64);
    let conflict = send(
        "PUT",
        path,
        b"two",
        "files-overwrite-key-01",
        Some(("If-Match", &bad_hash)),
    );
    management_reply_error(&conflict, 412, "precondition_failed");
    assert_eq!(conflict.json()["error"]["current_sha256"], digest(b"one"));
    let read = f.request("GET", path, b"", Some(&token));
    assert_eq!(read.status, 200);
    assert_eq!(read.body, b"one");
    assert_eq!(read.headers.get("x-vesta-sha256").unwrap(), &digest(b"one"));
    let hash = digest(b"one");
    let updated = send(
        "PUT",
        path,
        b"two",
        "files-overwrite-key-02",
        Some(("If-Match", &hash)),
    );
    assert_eq!(updated.status, 200);
    assert!(updated.json()["data"]["trash_id"].is_string());
    let hash = digest(b"two");
    let rename = serde_json::to_vec(
        &serde_json::json!({"from":"config/a.txt","to":"config/b.txt","expected_sha256":hash}),
    )
    .unwrap();
    let renamed = send(
        "POST",
        "/api/v3/manage/files/rename",
        &rename,
        "files-rename-key-0001",
        None,
    );
    assert_eq!(renamed.status, 200);
    assert_eq!(
        renamed.json()["data"],
        send(
            "POST",
            "/api/v3/manage/files/rename",
            &rename,
            "files-rename-key-0001",
            None
        )
        .json()["data"]
    );
    std::fs::write(f.temp.path().join("game/config/c.txt"), b"occupied").unwrap();
    let occupied = serde_json::to_vec(
        &serde_json::json!({"from":"config/b.txt","to":"config/c.txt","expected_sha256":hash}),
    )
    .unwrap();
    management_reply_error(
        &send(
            "POST",
            "/api/v3/manage/files/rename",
            &occupied,
            "files-rename-key-0002",
            None,
        ),
        412,
        "precondition_failed",
    );
    assert_eq!(
        std::fs::read(f.temp.path().join("game/config/c.txt")).unwrap(),
        b"occupied"
    );
    let deleted = send(
        "DELETE",
        "/api/v3/manage/files?path=config/b.txt",
        b"",
        "files-delete-key-0001",
        Some(("If-Match", &hash)),
    );
    assert_eq!(deleted.status, 200);
    assert!(!f.temp.path().join("game/config/b.txt").exists());
    assert_eq!(
        deleted.json()["data"],
        send(
            "DELETE",
            "/api/v3/manage/files?path=config/b.txt",
            b"",
            "files-delete-key-0001",
            Some(("If-Match", &hash))
        )
        .json()["data"]
    );
    let trash = f.request("GET", "/api/v3/manage/trash?limit=1", b"", Some(&token));
    assert_eq!(trash.status, 200);
    assert_eq!(trash.json()["data"]["items"].as_array().unwrap().len(), 1);
    assert!(trash.json()["data"]["next_cursor"].is_string());
    let id = deleted.json()["data"]["trash_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let restore = format!("/api/v3/manage/trash/{id}/restore");
    let restored = send("POST", &restore, b"{}", "files-restore-key-001", None);
    assert_eq!(restored.status, 200);
    assert_eq!(
        std::fs::read(f.temp.path().join("game/config/b.txt")).unwrap(),
        b"two"
    );
    assert_eq!(
        restored.json()["data"],
        send("POST", &restore, b"{}", "files-restore-key-001", None).json()["data"]
    );
    let mkdir = send(
        "POST",
        "/api/v3/manage/files/mkdir",
        br#"{"path":"config/child"}"#,
        "files-mkdir-key-00001",
        None,
    );
    assert_eq!(mkdir.status, 200);
    assert_eq!(
        mkdir.json()["data"],
        send(
            "POST",
            "/api/v3/manage/files/mkdir",
            br#"{"path":"config/child"}"#,
            "files-mkdir-key-00001",
            None
        )
        .json()["data"]
    );
    let audit = f.request("GET", "/api/v3/manage/audit?limit=200", b"", Some(&token));
    let value = audit.json();
    let items = value["data"]["items"].as_array().unwrap();
    for action in [
        "files.write",
        "files.rename",
        "files.delete",
        "files.restore",
        "files.mkdir",
    ] {
        assert!(items.iter().any(|e| e["action"] == action), "{action}");
    }
    audit.hides(&token);
    audit.hides(&f.temp.path().to_string_lossy());
}
#[test]
fn uploads_resume_finish_and_abort_idempotently() {
    let f = Fixture::new("minecraft", 8, 4096, 5);
    let (device, key) = enroll_management(&f, vesta_control::identity::BuiltinRole::Owner);
    let token = management_session(&f, &device, &key);
    let send =
        |method: &str, path: &str, body: &[u8], key: &str, condition: Option<(&str, &str)>| {
            send(&f, method, path, body, Some(&token), Some(key), condition)
        };
    let input=serde_json::to_vec(&serde_json::json!({"path":"config/upload.txt","total_size":"6","sha256":digest(b"abcdef")})).unwrap();
    let begun = send(
        "POST",
        "/api/v3/manage/files/uploads",
        &input,
        "upload-begin-key-0001",
        Some(("If-None-Match", "*")),
    );
    assert_eq!(begun.status, 200);
    assert_eq!(
        begun.json()["data"],
        send(
            "POST",
            "/api/v3/manage/files/uploads",
            &input,
            "upload-begin-key-0001",
            Some(("If-None-Match", "*"))
        )
        .json()["data"]
    );
    let id = begun.json()["data"]["upload_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let path = format!("/api/v3/manage/files/uploads/{id}");
    let append = send(
        "PUT",
        &format!("{path}?offset=0"),
        b"abc",
        "upload-append-key-001",
        None,
    );
    assert_eq!(append.status, 200);
    assert_eq!(append.json()["data"]["offset"], "3");
    assert_eq!(
        append.json()["data"],
        send(
            "PUT",
            &format!("{path}?offset=0"),
            b"abc",
            "upload-append-key-001",
            None
        )
        .json()["data"]
    );
    management_reply_error(
        &send(
            "PUT",
            &format!("{path}?offset=0"),
            b"xyz",
            "upload-append-key-001",
            None,
        ),
        409,
        "idempotency_conflict",
    );
    management_reply_error(
        &send(
            "PUT",
            &format!("{path}?offset=4"),
            b"d",
            "upload-append-key-gap",
            None,
        ),
        412,
        "precondition_failed",
    );
    assert_eq!(
        send(
            "PUT",
            &format!("{path}?offset=2"),
            b"cde",
            "upload-append-key-002",
            None
        )
        .json()["data"]["offset"],
        "5"
    );
    assert_eq!(
        send(
            "PUT",
            &format!("{path}?offset=5"),
            b"f",
            "upload-append-key-003",
            None
        )
        .status,
        200
    );
    let finished = send(
        "POST",
        &format!("{path}/finish"),
        b"{}",
        "upload-finish-key-001",
        None,
    );
    assert_eq!(finished.status, 200);
    assert_eq!(
        std::fs::read(f.temp.path().join("game/config/upload.txt")).unwrap(),
        b"abcdef"
    );
    assert_eq!(
        finished.json()["data"],
        send(
            "POST",
            &format!("{path}/finish"),
            b"{}",
            "upload-finish-key-001",
            None
        )
        .json()["data"]
    );
    assert_eq!(
        append.json()["data"],
        send(
            "PUT",
            &format!("{path}?offset=0"),
            b"abc",
            "upload-append-key-001",
            None
        )
        .json()["data"]
    );
    let begun = send(
        "POST",
        "/api/v3/manage/files/uploads",
        &input,
        "upload-begin-key-0002",
        Some(("If-None-Match", "*")),
    );
    let id = begun.json()["data"]["upload_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let path = format!("/api/v3/manage/files/uploads/{id}");
    let aborted = send("DELETE", &path, b"", "upload-abort-key-0001", None);
    assert_eq!(aborted.status, 200);
    assert_eq!(
        aborted.json()["data"],
        send("DELETE", &path, b"", "upload-abort-key-0001", None).json()["data"]
    );
    let audit = f.request("GET", "/api/v3/manage/audit?limit=200", b"", Some(&token));
    for action in [
        "files.upload.begin",
        "files.upload.append",
        "files.upload.abort",
    ] {
        assert!(
            audit.json()["data"]["items"]
                .as_array()
                .unwrap()
                .iter()
                .any(|e| e["action"] == action)
        );
    }
}
#[test]
fn file_queries_refuse_paths_aliases_and_stale_cursors() {
    let f = Fixture::new("minecraft", 8, 1_048_576, 5);
    let (device, key) = enroll_management(&f, vesta_control::identity::BuiltinRole::Owner);
    let token = management_session(&f, &device, &key);
    for path in [
        "../private/key.pem",
        "/etc/passwd",
        "C:/Windows/win.ini",
        "config%5Ca.txt",
        "config/../a.txt",
        ".vesta/secrets",
        ".git/config",
        "config/a.txt:ads",
        "config/CON",
        "config/a.txt.",
        "config//a.txt",
    ] {
        management_reply_error(
            &f.request(
                "GET",
                &format!("/api/v3/manage/files/content?path={path}"),
                b"",
                Some(&token),
            ),
            400,
            "invalid_request",
        );
    }
    for name in ["b.txt", "A.txt", "c.txt"] {
        std::fs::write(
            f.temp.path().join(format!("game/config/{name}")),
            b"fixture",
        )
        .unwrap();
    }
    let page = f.request(
        "GET",
        "/api/v3/manage/files?path=config&limit=1",
        b"",
        Some(&token),
    );
    assert_eq!(page.status, 200);
    assert_eq!(page.json()["data"]["items"][0]["name"], "A.txt");
    let cursor = page.json()["data"]["next_cursor"]
        .as_str()
        .unwrap()
        .to_owned();
    let next = f.request(
        "GET",
        &format!("/api/v3/manage/files?path=config&limit=1&cursor={cursor}"),
        b"",
        Some(&token),
    );
    assert_eq!(next.json()["data"]["items"][0]["name"], "b.txt");
    std::fs::write(f.temp.path().join("game/config/d.txt"), b"fixture").unwrap();
    management_reply_error(
        &f.request(
            "GET",
            &format!("/api/v3/manage/files?path=config&limit=1&cursor={cursor}"),
            b"",
            Some(&token),
        ),
        409,
        "cursor_gap",
    );
    for query in [
        "path=config&limit=101",
        "path=config&path=config",
        "path=config&unknown=1",
        "path=%ZZ",
    ] {
        management_reply_error(
            &f.request(
                "GET",
                &format!("/api/v3/manage/files?{query}"),
                b"",
                Some(&token),
            ),
            400,
            "invalid_request",
        );
    }
    let bytes = vec![b'x'; 128 * 1024];
    std::fs::write(f.temp.path().join("game/config/large.txt"), &bytes).unwrap();
    let read = f.request(
        "GET",
        "/api/v3/manage/files/content?path=config/large.txt",
        b"",
        Some(&token),
    );
    assert_eq!(read.status, 200);
    assert_eq!(read.body, bytes);
    std::fs::write(
        f.temp.path().join("game/config/too-large.txt"),
        vec![0; 1_048_577],
    )
    .unwrap();
    management_reply_error(
        &f.request(
            "GET",
            "/api/v3/manage/files/content?path=config/too-large.txt",
            b"",
            Some(&token),
        ),
        413,
        "request_too_large",
    );
    std::fs::hard_link(
        f.temp.path().join("private/key.pem"),
        f.temp.path().join("game/config/alias.pem"),
    )
    .unwrap();
    management_reply_error(
        &f.request(
            "GET",
            "/api/v3/manage/files/content?path=config/alias.pem",
            b"",
            Some(&token),
        ),
        400,
        "invalid_request",
    );
}

#[test]
fn properties_redaction_preservation_preconditions_and_confirmation_receipts() {
    let f = Fixture::new("minecraft", 8, 65_536, 5);
    let (device, key) = enroll_management(&f, vesta_control::identity::BuiltinRole::Owner);
    let token = management_session(&f, &device, &key);
    let native = f.temp.path().join("game/server.properties");
    let initial=b"# comment retained\r\nonline-mode=true\r\nmotd=old\r\nrcon.password=fixture-secret\r\nunknown=C:\\private\\data\r\naccepts-transfers=false\r\ngenerator-settings={}\r\nlevel-name=world\r\nmanagement-server-secret=fixture-management-secret\r\n";
    std::fs::write(&native, initial).unwrap();
    let endpoint = "/api/v3/manage/config/server-properties";
    let read = f.request("GET", endpoint, b"", Some(&token));
    assert_eq!(read.status, 200);
    assert_eq!(read.json()["data"]["values"]["rcon.password"], "[redacted]");
    assert_eq!(read.json()["data"]["values"]["unknown"], "[redacted]");
    assert_eq!(read.json()["data"]["values"]["accepts-transfers"], false);
    assert_eq!(read.json()["data"]["values"]["generator-settings"], "{}");
    assert_eq!(read.json()["data"]["values"]["level-name"], "world");
    assert_eq!(
        read.json()["data"]["values"]["management-server-secret"],
        "[redacted]"
    );
    read.hides("fixture-management-secret");
    read.hides("fixture-secret");
    read.hides("C:\\private");
    let safe = serde_json::json!({"changes":{"motd":"new title","view-distance":12},"expected_sha256":digest(initial)});
    let bytes = serde_json::to_vec(&safe).unwrap();
    let updated = send(
        &f,
        "PATCH",
        endpoint,
        &bytes,
        Some(&token),
        Some("properties-safe-key-01"),
        None,
    );
    assert_eq!(updated.status, 200);
    assert_eq!(updated.json()["data"]["restart_needed"], true);
    assert_eq!(
        updated.json()["data"],
        send(
            &f,
            "PATCH",
            endpoint,
            &bytes,
            Some(&token),
            Some("properties-safe-key-01"),
            None
        )
        .json()["data"]
    );
    let current = std::fs::read(&native).unwrap();
    let text = String::from_utf8(current.clone()).unwrap();
    assert!(text.starts_with("# comment retained\r\n"));
    assert!(text.contains("unknown=C:\\private\\data\r\n"));
    assert!(text.contains("rcon.password=fixture-secret\r\n"));
    let stale = send(
        &f,
        "PATCH",
        endpoint,
        &bytes,
        Some(&token),
        Some("properties-stale-key1"),
        None,
    );
    management_reply_error(&stale, 412, "precondition_failed");
    assert_eq!(stale.json()["error"]["current_sha256"], digest(&current));
    for changes in [
        serde_json::json!({"unknown":true}),
        serde_json::json!({"online-mode":"false"}),
        serde_json::json!({"view-distance":1000}),
        serde_json::json!({"motd":"C:\\secret"}),
    ] {
        let body = serde_json::to_vec(
            &serde_json::json!({"changes":changes,"expected_sha256":digest(&current)}),
        )
        .unwrap();
        management_reply_error(
            &send(
                &f,
                "PATCH",
                endpoint,
                &body,
                Some(&token),
                Some("properties-invalid-01"),
                None,
            ),
            400,
            "invalid_request",
        );
    }
    let mut weak = serde_json::json!({"changes":{"online-mode":false,"enable-rcon":true,"enable-query":true,"enable-command-block":true},"expected_sha256":digest(&current)});
    let body = serde_json::to_vec(&weak).unwrap();
    let prepared = send(
        &f,
        "PATCH",
        endpoint,
        &body,
        Some(&token),
        Some("properties-weak-key01"),
        None,
    );
    assert_eq!(prepared.status, 202);
    assert_eq!(prepared.json()["data"]["state"], "awaiting_confirmation");
    assert_eq!(std::fs::read(&native).unwrap(), current);
    management_reply_error(
        &send(
            &f,
            "PATCH",
            endpoint,
            &bytes,
            Some(&token),
            Some("properties-weak-key01"),
            None,
        ),
        409,
        "idempotency_conflict",
    );
    weak["confirmation_token"] = prepared.json()["data"]["confirmation_token"].clone();
    let final_body = serde_json::to_vec(&weak).unwrap();
    let completed = send(
        &f,
        "PATCH",
        endpoint,
        &final_body,
        Some(&token),
        Some("properties-weak-key01"),
        None,
    );
    assert_eq!(completed.status, 200);
    assert_eq!(completed.json()["data"]["restart_needed"], true);
    assert_eq!(
        completed.json()["data"],
        send(
            &f,
            "PATCH",
            endpoint,
            &final_body,
            Some(&token),
            Some("properties-weak-key01"),
            None
        )
        .json()["data"]
    );
    let applied = String::from_utf8(std::fs::read(&native).unwrap()).unwrap();
    assert!(applied.contains("online-mode=false"));
    assert!(applied.contains("enable-rcon=true"));
    assert_eq!(
        f.request("GET", "/api/v3/manage/server/status", b"", Some(&token))
            .json()["data"]["state"],
        "stopped"
    );
    let audit = f.request("GET", "/api/v3/manage/audit?limit=200", b"", Some(&token));
    assert!(
        audit.json()["data"]["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["action"] == "config.server_properties")
    );
    audit.hides("fixture-secret");
    audit.hides(weak["confirmation_token"].as_str().unwrap());
    management_reply_error(
        &send(
            &f,
            "PUT",
            "/api/v3/manage/files/content?path=server.properties",
            initial,
            Some(&token),
            Some("properties-bypass-001"),
            Some(("If-None-Match", "*")),
        ),
        400,
        "invalid_request",
    );
    let trash = updated.json()["data"]["trash_id"]
        .as_str()
        .unwrap()
        .to_owned();
    std::fs::remove_file(&native).unwrap();
    management_reply_error(
        &send(
            &f,
            "POST",
            &format!("/api/v3/manage/trash/{trash}/restore"),
            b"{}",
            Some(&token),
            Some("properties-bypass-002"),
            None,
        ),
        404,
        "resource_not_found",
    );
}

#[test]
fn each_file_endpoint_checks_its_exact_capability() {
    use vesta_control::identity::{BuiltinRole, Capability};
    for (method, route, capability) in [
        ("GET", "files", Capability::FilesList),
        ("GET", "files/content", Capability::FilesRead),
        ("PUT", "files/create", Capability::FilesCreate),
        ("PUT", "files/overwrite", Capability::FilesOverwrite),
        ("DELETE", "files", Capability::FilesDelete),
        ("POST", "files/mkdir", Capability::FilesMkdir),
        ("POST", "files/rename", Capability::FilesRename),
        ("POST", "files/uploads", Capability::FilesCreate),
        ("PUT", "append", Capability::FilesCreate),
        ("POST", "finish", Capability::FilesCreate),
        ("DELETE", "abort", Capability::FilesCreate),
        ("GET", "trash", Capability::FilesRead),
        ("POST", "restore", Capability::FilesCreate),
        ("POST", "content/install", Capability::ContentInstall),
        ("POST", "content/install", Capability::FilesCreate),
        ("GET", "config/server-properties", Capability::ConfigView),
        ("GET", "config/server-properties", Capability::FilesRead),
        ("PATCH", "config/server-properties", Capability::ConfigEdit),
        (
            "PATCH",
            "config/server-properties",
            Capability::FilesOverwrite,
        ),
        ("PATCH", "config/server-properties", Capability::FilesRead),
    ] {
        let f = Fixture::new("minecraft", 8, 4096, 5);
        let (device, key) = enroll_management(&f, BuiltinRole::Owner);
        let token = management_session(&f, &device, &key);
        let subject = f.host().management().authenticate(&token).unwrap();
        std::fs::write(f.temp.path().join("game/config/file.txt"), b"fixture").unwrap();
        std::fs::write(
            f.temp.path().join("game/server.properties"),
            b"online-mode=true\n",
        )
        .unwrap();
        let upload = f
            .host()
            .files()
            .begin(
                &subject,
                &vesta_control::files::RelativePath::parse("config/new.txt").unwrap(),
                &vesta_control::files::WritePrecondition::Absent,
                0,
                &digest(b""),
            )
            .unwrap();
        let deleted = f
            .host()
            .files()
            .delete(
                &subject,
                &vesta_control::files::RelativePath::parse("config/file.txt").unwrap(),
                &digest(b"fixture"),
            )
            .unwrap();
        std::fs::write(f.temp.path().join("game/config/file.txt"), b"fixture").unwrap();
        let mut condition = None;
        let route = match route {
            "files/create" => {
                condition = Some(("If-None-Match", "*".to_owned()));
                "files/content?path=config/new.txt".to_owned()
            }
            "files/overwrite" => {
                condition = Some(("If-Match", digest(b"fixture")));
                "files/content?path=config/file.txt".to_owned()
            }
            "files/content" => "files/content?path=config/file.txt".into(),
            "files" if method == "DELETE" => {
                condition = Some(("If-Match", digest(b"fixture")));
                "files?path=config/file.txt".into()
            }
            "files/uploads" => {
                condition = Some(("If-None-Match", "*".to_owned()));
                "files/uploads".into()
            }
            "append" => format!("files/uploads/{}?offset=0", upload.as_str()),
            "finish" => format!("files/uploads/{}/finish", upload.as_str()),
            "abort" => format!("files/uploads/{}", upload.as_str()),
            "restore" => format!("trash/{}/restore", deleted.id),
            _ => route.into(),
        };
        let condition = condition
            .as_ref()
            .map(|(name, value)| (*name, value.as_str()));
        let body: &[u8] = if matches!(method, "POST" | "PATCH") {
            b"{}"
        } else {
            b""
        };
        let db = f.host().database();
        db.write(|c| { c.execute("INSERT INTO principal_overrides (id,principal_id,capability,scope,effect,granted_by,created_at) VALUES ('11111111-2222-4333-8444-555555555555',?,?,'*','deny','fixture',?)",rusqlite::params![subject.principal.as_str(),capability.id(),db.clock().millis()])?; Ok(()) }).unwrap();
        management_reply_error(
            &send(
                &f,
                method,
                &format!("/api/v3/manage/{route}"),
                body,
                Some(&token),
                Some("endpoint-denial-key01"),
                condition,
            ),
            403,
            "management_forbidden",
        );
        db.write(|c| {
            c.execute("DELETE FROM principal_overrides",[])?;
            c.execute("INSERT INTO roles (id,name,rank,builtin,created_by,created_at) VALUES ('file-cap-absence','File cap absence',1,0,'fixture',?)",[db.clock().millis()])?;
            c.execute("INSERT INTO role_capabilities (role_id,capability,scope) SELECT 'file-cap-absence',capability,scope FROM role_capabilities WHERE role_id='owner' AND capability<>?",[capability.id()])?;
            c.execute("UPDATE role_assignments SET role_id='file-cap-absence' WHERE principal_id=?",[subject.principal.as_str()])?; Ok(())
        }).unwrap();
        management_reply_error(
            &send(
                &f,
                method,
                &format!("/api/v3/manage/{route}"),
                body,
                Some(&token),
                Some("endpoint-denial-key01"),
                condition,
            ),
            403,
            "management_forbidden",
        );
    }
}

#[test]
fn properties_receipts_burn_refusals_and_roll_back_with_failed_audit() {
    let f = Fixture::new("minecraft", 8, 65_536, 5);
    let (device, key) = enroll_management(&f, vesta_control::identity::BuiltinRole::Owner);
    let token = management_session(&f, &device, &key);
    let (other_device, other_key) =
        enroll_management(&f, vesta_control::identity::BuiltinRole::Owner);
    let other = management_session(&f, &other_device, &other_key);
    let native = f.temp.path().join("game/server.properties");
    let initial = b"# retained\nonline-mode=true\n";
    std::fs::write(&native, initial).unwrap();
    let endpoint = "/api/v3/manage/config/server-properties";
    let intent =
        serde_json::json!({"changes":{"online-mode":false},"expected_sha256":digest(initial)});
    let prepare = |request_key: &str| {
        send(
            &f,
            "PATCH",
            endpoint,
            &serde_json::to_vec(&intent).unwrap(),
            Some(&token),
            Some(request_key),
            None,
        )
    };
    let first = prepare("receipt-supersede-001");
    assert_eq!(first.status, 202);
    let second = prepare("receipt-supersede-001");
    assert_eq!(second.status, 202);
    let with_token = |receipt: &WireReply| {
        let mut value = intent.clone();
        value["confirmation_token"] = receipt.json()["data"]["confirmation_token"].clone();
        serde_json::to_vec(&value).unwrap()
    };
    management_reply_error(
        &send(
            &f,
            "PATCH",
            endpoint,
            &with_token(&first),
            Some(&token),
            Some("receipt-supersede-001"),
            None,
        ),
        409,
        "confirmation_invalid",
    );
    let wrong_device = with_token(&second);
    management_reply_error(
        &send(
            &f,
            "PATCH",
            endpoint,
            &wrong_device,
            Some(&other),
            Some("receipt-wrong-device1"),
            None,
        ),
        409,
        "confirmation_invalid",
    );
    management_reply_error(
        &send(
            &f,
            "PATCH",
            endpoint,
            &wrong_device,
            Some(&token),
            Some("receipt-supersede-001"),
            None,
        ),
        409,
        "confirmation_invalid",
    );
    assert_eq!(std::fs::read(&native).unwrap(), initial);
    let fresh = prepare("receipt-atomic-key01");
    assert_eq!(fresh.status, 202);
    let approved = with_token(&fresh);
    let db = f.host().database();
    db.write(|c| { c.execute_batch("CREATE TRIGGER refuse_config_audit BEFORE INSERT ON audit_events WHEN NEW.action='config.server_properties' AND NEW.outcome='success' BEGIN SELECT RAISE(ABORT,'fixture'); END;")?; Ok(()) }).unwrap();
    management_reply_error(
        &send(
            &f,
            "PATCH",
            endpoint,
            &approved,
            Some(&token),
            Some("receipt-atomic-key01"),
            None,
        ),
        503,
        "temporarily_unavailable",
    );
    assert_eq!(std::fs::read(&native).unwrap(), initial);
    db.write(|c| {
        c.execute_batch("DROP TRIGGER refuse_config_audit")?;
        Ok(())
    })
    .unwrap();
    let completed = send(
        &f,
        "PATCH",
        endpoint,
        &approved,
        Some(&token),
        Some("receipt-atomic-key01"),
        None,
    );
    assert_eq!(completed.status, 200);
    assert_eq!(
        completed.json()["data"],
        send(
            &f,
            "PATCH",
            endpoint,
            &approved,
            Some(&token),
            Some("receipt-atomic-key01"),
            None
        )
        .json()["data"]
    );
    let denied:i64=db.read(|c| Ok(c.query_row("SELECT count(*) FROM audit_events WHERE action='confirmation.accept' AND outcome='denied'",[],|r|r.get(0))?)).unwrap();
    assert_eq!(denied, 3);
    assert!(
        vesta_state::audit::AuditLog::new(db.clone())
            .unwrap()
            .verify_chain()
            .unwrap()
            .valid
    );
    std::fs::write(&native, initial).unwrap();
    let expired = prepare("receipt-expired-key1");
    assert_eq!(expired.status, 202);
    f.clock
        .0
        .fetch_add(120_001, std::sync::atomic::Ordering::SeqCst);
    management_reply_error(
        &send(
            &f,
            "PATCH",
            endpoint,
            &with_token(&expired),
            Some(&token),
            Some("receipt-expired-key1"),
            None,
        ),
        409,
        "confirmation_invalid",
    );
    assert_eq!(std::fs::read(&native).unwrap(), initial);
}

#[test]
fn owner_and_admin_raw_documents_require_confirmation_and_preserve_private_hiding() {
    for role in [
        vesta_control::identity::BuiltinRole::Owner,
        vesta_control::identity::BuiltinRole::Administrator,
    ] {
        let f = Fixture::new("minecraft", 8, 1_048_576, 5);
        std::fs::write(f.temp.path().join("game/ops.json"), b"[]").unwrap();
        let (device, key) = enroll_management(&f, role);
        let token = management_session(&f, &device, &key);
        let path = "/api/v3/manage/files/content?path=ops.json";
        assert_eq!(
            send(&f, "GET", path, b"", Some(&token), None, None).status,
            200
        );
        let expected = digest(b"[]");
        let intent = "raw-protected-confirm-0001";
        let pending = send(
            &f,
            "PUT",
            path,
            b"[{}]",
            Some(&token),
            Some(intent),
            Some(("If-Match", &expected)),
        );
        assert_eq!(pending.status, 200);
        let pending = pending.json();
        assert_eq!(pending["data"]["state"], "awaiting_confirmation");
        assert_eq!(
            std::fs::read(f.temp.path().join("game/ops.json")).unwrap(),
            b"[]"
        );
        let receipt = pending["data"]["confirmation_token"].as_str().unwrap();
        let confirmed = format!("{path}&confirmation_token={receipt}");
        let outcome = send(
            &f,
            "PUT",
            &confirmed,
            b"[{}]",
            Some(&token),
            Some(intent),
            Some(("If-Match", &expected)),
        );
        assert_eq!(outcome.status, 200);
        assert_eq!(outcome.json()["data"]["state"], "applied");
        assert_eq!(
            send(
                &f,
                "PUT",
                &confirmed,
                b"[{}]",
                Some(&token),
                Some(intent),
                Some(("If-Match", &expected))
            )
            .json()["data"],
            outcome.json()["data"]
        );
        management_reply_error(
            &send(
                &f,
                "GET",
                "/api/v3/manage/files/content?path=.vesta/private",
                b"",
                Some(&token),
                None,
                None,
            ),
            400,
            "invalid_request",
        );
    }
}

#[test]
fn eula_route_reads_writes_and_replays_explicit_consent() {
    let f = Fixture::new("minecraft", 8, 65536, 5);
    let (device, key) = enroll_management(&f, vesta_control::identity::BuiltinRole::Owner);
    let token = management_session(&f, &device, &key);
    let get = || {
        send(
            &f,
            "GET",
            "/api/v3/manage/eula",
            b"",
            Some(&token),
            None,
            None,
        )
    };
    assert_eq!(get().json()["data"]["accepted"], false);
    for _ in 0..2 {
        let reply = send(
            &f,
            "POST",
            "/api/v3/manage/eula",
            br#"{"accepted":true}"#,
            Some(&token),
            Some("accept-eula-explicit"),
            None,
        );
        assert_eq!(reply.status, 200);
        assert_eq!(reply.json()["data"]["accepted"], true);
    }
    assert_eq!(get().json()["data"]["accepted"], true);
    assert!(
        std::fs::read_to_string(f.temp.path().join("game/eula.txt"))
            .unwrap()
            .contains("eula=true")
    );
    assert_eq!(
        send(
            &f,
            "POST",
            "/api/v3/manage/eula",
            br#"{"accepted":false,"extra":true}"#,
            Some(&token),
            Some("bad-eula-explicit"),
            None
        )
        .status,
        400
    );
}
