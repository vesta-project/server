//! Real authenticated TLS workspace lifecycle; no provider requests or game launch.
use super::*;
use serde_json::json;
#[test]
fn signed_pack_sync_binds_freshness_identity_policy_and_latest_download() {
    use base64::{Engine, engine::general_purpose::STANDARD};
    let f = Fixture::new("minecraft", 8, 65536, 5);
    let (device, key) = enroll_management(&f, vesta_control::identity::BuiltinRole::Owner);
    let token = management_session(&f, &device, &key);
    let initial = f.request("GET", "/api/v3/manage/pack/draft", b"", Some(&token));
    let manifest = initial.json()["data"]["draft"]["manifest"].clone();
    let saved = management_mutation(
        &f,
        &token,
        "pack/draft",
        "sync-edit-00000001",
        &json!({"expected_revision":"0","manifest":manifest}),
    );
    assert_eq!(saved.status, 200);
    let preview = management_mutation(
        &f,
        &token,
        "pack/preview",
        "sync-preview-0001",
        &json!({"draft_revision":"1","resolved_entries":[]}),
    );
    assert_eq!(preview.status, 200);
    let published = management_mutation(
        &f,
        &token,
        "pack/publish",
        "sync-publish-0001",
        &json!({"expected_revision":"0","draft_revision":"1","resolved_entries":[],"review_sha256":preview.json()["data"]["review_sha256"],"required_update":true,"acknowledge_lower_version":false}),
    );
    assert_eq!(published.status, 200);
    let history = f.request("GET", "/api/v3/manage/pack/history", b"", Some(&token));
    assert_eq!(history.status, 200);
    assert_eq!(history.json()["data"][0]["revision"], "1");
    assert_eq!(history.json()["data"][0]["required_update"], true);
    assert_eq!(
        f.request("GET", "/api/v3/manage/pack/history", b"", None)
            .status,
        401
    );
    let path = "/api/v3/manage/pack/sync?installed=0&nonce=0123456789abcdef0123456789abcdef";
    assert_eq!(f.request("GET", path, b"", None).status, 401);
    // Publishing a custom workspace does not implicitly offer it to players.
    assert_eq!(f.request("GET", path, b"", Some(&token)).status, 404);
    let mode = management_mutation(
        &f,
        &token,
        "pack/client-mode",
        "sync-mode-custom-1",
        &json!({"mode":"custom","require_update":false}),
    );
    assert_eq!(mode.status, 200);
    let response = f.request("GET", path, b"", Some(&token));
    assert_eq!(
        response.status,
        200,
        "{}",
        String::from_utf8_lossy(&response.body)
    );
    let envelope = response.json()["data"].clone();
    let payload: serde_json::Value = serde_json::from_slice(
        &STANDARD
            .decode(envelope["payload_base64"].as_str().unwrap())
            .unwrap(),
    )
    .unwrap();
    assert_eq!(payload["revision"], "1");
    assert_eq!(payload["minimum_revision"], "1");
    assert_eq!(payload["status"], "update_required");
    assert_eq!(payload["server_id"], f.host().management().server_id());
    assert_eq!(payload["nonce"], "0123456789abcdef0123456789abcdef");
    assert!(
        !STANDARD
            .decode(envelope["signature_base64"].as_str().unwrap())
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        payload["fingerprint"],
        f.transport.as_ref().unwrap().tls_fingerprint().unwrap()
    );
    let download = format!(
        "/api/v3/manage/pack/artifact?revision=1&sha256={}",
        payload["sha256"].as_str().unwrap()
    );
    let artifact = f.request("GET", &download, b"", Some(&token));
    assert_eq!(artifact.status, 200);
    assert_eq!(
        artifact.body.len().to_string(),
        payload["size"].as_str().unwrap()
    );
    let stale = download.replace("revision=1", "revision=0");
    assert_eq!(f.request("GET", &stale, b"", Some(&token)).status, 412);
    assert_eq!(
        f.request(
            "GET",
            &path.replace("installed=0", "installed=2"),
            b"",
            Some(&token)
        )
        .status,
        409
    );
    assert_eq!(
        f.request(
            "GET",
            &path.replace("installed=0", "installed=01"),
            b"",
            Some(&token)
        )
        .status,
        400
    );
    assert_eq!(f.request("POST", path, b"", Some(&token)).status, 405);
    let (player, key) = enroll_management(&f, vesta_control::identity::BuiltinRole::PackUser);
    let player_token = management_session(&f, &player, &key);
    assert_eq!(f.request("GET", path, b"", Some(&player_token)).status, 200);
    assert_eq!(
        f.request("GET", "/api/v3/manage/pack/draft", b"", Some(&player_token))
            .status,
        403
    );
    let preview = management_mutation(
        &f,
        &token,
        "pack/preview",
        "sync-preview-0002",
        &json!({"draft_revision":"1","resolved_entries":[]}),
    );
    assert_eq!(preview.status, 200);
    let optional = management_mutation(
        &f,
        &token,
        "pack/publish",
        "sync-publish-0002",
        &json!({"expected_revision":"1","draft_revision":"1","resolved_entries":[],"review_sha256":preview.json()["data"]["review_sha256"],"required_update":false,"acknowledge_lower_version":false}),
    );
    assert_eq!(optional.status, 200);
    let status_path = path.replace("installed=0", "installed=1");
    let decode = |reply: WireReply| -> serde_json::Value {
        assert_eq!(reply.status, 200);
        serde_json::from_slice(
            &STANDARD
                .decode(reply.json()["data"]["payload_base64"].as_str().unwrap())
                .unwrap(),
        )
        .unwrap()
    };
    let optional = decode(f.request("GET", &status_path, b"", Some(&player_token)));
    assert_eq!(optional["revision"], "2");
    assert_eq!(optional["minimum_revision"], "1");
    assert_eq!(optional["status"], "update_available");
    let required = management_mutation(
        &f,
        &token,
        "pack/minimum",
        "sync-minimum-0002",
        &json!({"expected_revision":"2","minimum_revision":"2"}),
    );
    assert_eq!(required.status, 200);
    let required = decode(f.request("GET", &status_path, b"", Some(&player_token)));
    assert_eq!(required["minimum_revision"], "2");
    assert_eq!(required["status"], "update_required");
    let current = decode(f.request(
        "GET",
        &path.replace("installed=0", "installed=2"),
        b"",
        Some(&player_token),
    ));
    assert_eq!(current["status"], "current");
}
#[test]
fn workspace_routes_separate_client_loader_draft_and_atomic_release() {
    let f = Fixture::new("minecraft", 8, 65536, 5);
    let (device, key) = enroll_management(&f, vesta_control::identity::BuiltinRole::Owner);
    let token = management_session(&f, &device, &key);
    assert_eq!(
        f.request("GET", "/api/v3/manage/pack/draft", b"", None)
            .status,
        401
    );
    let initial = f.request("GET", "/api/v3/manage/pack/draft", b"", Some(&token));
    assert_eq!(initial.status, 200);
    let mut manifest = initial.json()["data"]["draft"]["manifest"].clone();
    manifest["loader"] = json!("fabric");
    manifest["loader_version"] = json!("0.16.10");
    let input = json!({"expected_revision":"0","manifest":manifest});
    let saved = management_mutation(&f, &token, "pack/draft", "workspace-edit-001", &input);
    assert_eq!(
        saved.status,
        200,
        "{}",
        String::from_utf8_lossy(&saved.body)
    );
    assert_eq!(saved.json()["data"]["revision"], "1");
    assert_eq!(
        management_mutation(&f, &token, "pack/draft", "workspace-edit-001", &input).json()["data"],
        saved.json()["data"]
    );
    assert_eq!(
        management_mutation(&f, &token, "pack/draft", "workspace-edit-002", &input).status,
        412
    );
    assert!(f.host().releases().current().unwrap().is_none());

    let preview = management_mutation(
        &f,
        &token,
        "pack/preview",
        "workspace-review-1",
        &json!({"draft_revision":"1","resolved_entries":[]}),
    );
    assert_eq!(
        preview.status,
        200,
        "{}",
        String::from_utf8_lossy(&preview.body)
    );
    assert_eq!(preview.json()["data"]["valid"], true);
    let publish = json!({"expected_revision":"0","draft_revision":"1","resolved_entries":[],"review_sha256":preview.json()["data"]["review_sha256"],"required_update":true,"acknowledge_lower_version":false});
    let first = management_mutation(&f, &token, "pack/publish", "workspace-publish1", &publish);
    assert_eq!(
        first.status,
        200,
        "{}",
        String::from_utf8_lossy(&first.body)
    );
    assert_eq!(first.json()["data"]["hosted_revision"], "1");
    assert_eq!(first.json()["data"]["minimum_revision"], "1");
    assert_eq!(
        management_mutation(&f, &token, "pack/publish", "workspace-publish1", &publish).json()["data"],
        first.json()["data"]
    );
    let active = f.host().releases().current().unwrap().unwrap();
    let loaded = f
        .host()
        .releases()
        .artifacts()
        .load(active.artifact(), vesta_control::packs::MAX_ARTIFACT_BYTES)
        .unwrap();
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(loaded.bytes())).unwrap();
    let index: serde_json::Value =
        serde_json::from_reader(archive.by_name(vesta_protocol::pack::INDEX_NAME).unwrap())
            .unwrap();
    assert_eq!(index["modloaderType"], "fabric");
    let from_release = management_mutation(
        &f,
        &token,
        "pack/draft/import",
        "workspace-import-1",
        &json!({"source":"published","expected_revision":"1","expected_published_revision":"1"}),
    );
    assert_eq!(from_release.status, 200);
    assert_eq!(from_release.json()["data"]["revision"], "2");
    assert_eq!(
        f.host().releases().current().unwrap().unwrap().revision(),
        1
    );
}
#[test]
fn workspace_denies_pack_users_and_rechecks_build_capability() {
    let f = Fixture::new("minecraft", 8, 65536, 5);
    let (device, key) = enroll_management(&f, vesta_control::identity::BuiltinRole::Viewer);
    let token = management_session(&f, &device, &key);
    management_reply_error(
        &f.request("GET", "/api/v3/manage/pack/draft", b"", Some(&token)),
        403,
        "management_forbidden",
    );
    management_reply_error(
        &management_mutation(&f, &token, "pack/draft", "workspace-denied-1", &json!({})),
        403,
        "management_forbidden",
    );
    let (owner_device, owner_key) =
        enroll_management(&f, vesta_control::identity::BuiltinRole::Owner);
    let owner_token = management_session(&f, &owner_device, &owner_key);
    let subject = f
        .host()
        .management()
        .authorize(
            &owner_token,
            vesta_control::identity::Capability::PackEdit,
            None,
        )
        .unwrap();
    f.host().database().write(|c|{c.execute("INSERT INTO principal_overrides(id,principal_id,capability,scope,effect,granted_by,created_at) VALUES('workspace-build-deny',?,'pack.build','*','deny','fixture',?)",rusqlite::params![subject.principal.as_str(),f.host().database().clock().millis()])?;Ok(())}).unwrap();
    management_reply_error(
        &management_mutation(
            &f,
            &owner_token,
            "pack/preview",
            "workspace-build-deny",
            &json!({}),
        ),
        403,
        "management_forbidden",
    );
}

#[test]
fn server_follow_delivery_uses_installed_provider_and_monotonic_mode_switches_over_https() {
    use base64::{Engine, engine::general_purpose::STANDARD};
    let f = Fixture::new("minecraft", 8, 65536, 5);
    let (device, key) = enroll_management(&f, vesta_control::identity::BuiltinRole::Owner);
    let token = management_session(&f, &device, &key);
    let db = f.host().database();
    // Exercise the public routes against durable installation receipts. Provider IDs
    // are intentionally non-resolvable: delivering them must not fetch the provider.
    f.request("GET", "/api/v3/manage/pack/draft", b"", Some(&token));
    let source = |version: &str| json!({"source":"provider","provider":"modrinth","project_id":"pack","version_id":version});
    let link = json!({"source":source("pending"),"display_version":"2","files":{},"state":"staged_until_restart","pending_ids":["not-applied"],"installed":{"source":source("installed"),"display_version":"1"},"pending":{"source":source("pending"),"display_version":"2"}});
    db.write(|c| {
        c.execute(
            "INSERT INTO server_modpack_link VALUES(?,?)",
            rusqlite::params![db.server_id(), link.to_string()],
        )?;
        Ok(())
    })
    .unwrap();
    let mode = |name: &str, required: bool, intent: &str| {
        let response = management_mutation(
            &f,
            &token,
            "pack/client-mode",
            &format!("{intent}-00000001"),
            &json!({"mode":name,"require_update":required}),
        );
        assert_eq!(
            response.status,
            200,
            "{}",
            String::from_utf8_lossy(&response.body)
        );
    };
    mode("server-modpack", false, "delivery-follow-1");
    let path = "/api/v3/manage/pack/sync?installed=0&nonce=0123456789abcdef0123456789abcdef";
    let sync = || {
        let reply = f.request("GET", path, b"", Some(&token));
        assert_eq!(
            reply.status,
            200,
            "{}",
            String::from_utf8_lossy(&reply.body)
        );
        serde_json::from_slice::<serde_json::Value>(
            &STANDARD
                .decode(reply.json()["data"]["payload_base64"].as_str().unwrap())
                .unwrap(),
        )
        .unwrap()
    };
    let first = sync();
    assert_eq!(first["format"], "catalogue");
    let download = format!(
        "/api/v3/manage/pack/artifact?revision={}&sha256={}",
        first["revision"].as_str().unwrap(),
        first["sha256"].as_str().unwrap()
    );
    let bytes = f.request("GET", &download, b"", Some(&token));
    assert_eq!(bytes.status, 200);
    let catalogue: serde_json::Value = serde_json::from_slice(&bytes.body).unwrap();
    assert_eq!(catalogue["content"]["versionId"], "installed");
    assert!(bytes.body.len() < 1024);
    mode("none", false, "delivery-none-1");
    assert_eq!(f.request("GET", path, b"", Some(&token)).status, 404);
    assert_eq!(
        f.request("GET", "/api/v2/pack/status?installed=0", b"", Some(&token))
            .json()["published"],
        false
    );
    mode("server-modpack", false, "delivery-follow-2");
    assert_eq!(sync()["sha256"], first["sha256"]);
    db.write(|c| {
        c.execute(
            "UPDATE server_modpack_link SET body=json_set(body,'$.pending_ids',json('[]'))",
            [],
        )?;
        Ok(())
    })
    .unwrap();
    let second = sync();
    assert!(
        second["revision"].as_str().unwrap().parse::<u64>().unwrap()
            > first["revision"].as_str().unwrap().parse::<u64>().unwrap()
    );
    assert_eq!(second["minimum_revision"], first["minimum_revision"]);
    assert_eq!(f.request("GET", &download, b"", Some(&token)).status, 412);
    mode("server-modpack", true, "delivery-required-1");
    let required = sync();
    assert_eq!(required["minimum_revision"], second["revision"]);
    mode("server-modpack", false, "delivery-optional-1");
    assert_eq!(sync()["minimum_revision"], required["minimum_revision"]);
    assert_eq!(
        f.request("GET", "/api/v2/pack/artifact", b"", Some(&token))
            .status,
        200
    );
    assert!(f.host().releases().current().unwrap().is_none());
    let custom = management_mutation(
        &f,
        &token,
        "pack/provider/publish",
        "delivery-custom-publication-1",
        &json!({"expected_revision":"0","draft_revision":"0","provider":"modrinth","project_id":"custom-pack","version_id":"custom-version","required_update":false,"acknowledge_lower_version":false}),
    );
    assert_eq!(
        custom.status,
        200,
        "{}",
        String::from_utf8_lossy(&custom.body)
    );
    mode("custom", false, "delivery-mode-custom");
    let custom_offered = sync();
    assert!(
        custom_offered["revision"]
            .as_str()
            .unwrap()
            .parse::<u64>()
            .unwrap()
            > required["revision"]
                .as_str()
                .unwrap()
                .parse::<u64>()
                .unwrap()
    );
    mode("server-modpack", false, "delivery-mode-follow-again");
    let following = sync();
    assert!(
        following["revision"]
            .as_str()
            .unwrap()
            .parse::<u64>()
            .unwrap()
            > custom_offered["revision"]
                .as_str()
                .unwrap()
                .parse::<u64>()
                .unwrap()
    );
    assert_eq!(following["minimum_revision"], required["minimum_revision"]);
    assert_eq!(
        f.host().releases().current().unwrap().unwrap().revision(),
        1
    );
    // Hosted archive installations use the same signed route and pinned download,
    // while the upload itself may already have been deleted.
    let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    zip.start_file("vesta.index.json", zip::write::SimpleFileOptions::default())
        .unwrap();
    zip.write_all(br#"{"formatVersion":1,"revision":1,"minimumRevision":1,"name":"Archive","minecraftVersion":"1.21.1","files":[]}"#).unwrap();
    let archive = zip.finish().unwrap().into_inner();
    let source = json!({"source":"uploaded_archive","upload_id":"deleted-upload"});
    let link = json!({"source":source,"display_version":"archive","files":{},"state":"installed","pending_ids":[]});
    db.write(|c| {
        c.execute("UPDATE server_modpack_link SET body=?", [link.to_string()])?;
        c.execute(
            "INSERT INTO server_modpack_delivery VALUES(?,?,?)",
            rusqlite::params![db.server_id(), source.to_string(), archive],
        )?;
        Ok(())
    })
    .unwrap();
    let hosted = sync();
    assert_eq!(hosted["format"], "vestapack");
    let url = format!(
        "/api/v3/manage/pack/artifact?revision={}&sha256={}",
        hosted["revision"].as_str().unwrap(),
        hosted["sha256"].as_str().unwrap()
    );
    let artifact = f.request("GET", &url, b"", Some(&token));
    assert_eq!(artifact.status, 200);
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(artifact.body)).unwrap();
    let index: serde_json::Value =
        serde_json::from_reader(zip.by_name("vesta.index.json").unwrap()).unwrap();
    assert_eq!(
        index["revision"].as_u64().unwrap().to_string(),
        hosted["revision"].as_str().unwrap()
    );
    assert_eq!(index["name"], "Archive");
}

#[test]
fn authenticated_pack_chunks_exceed_default_body_cap_without_relaxing_anonymous_requests() {
    use base64::{Engine, engine::general_purpose::STANDARD};
    use sha2::{Digest, Sha256};
    let f = Fixture::new("minecraft", 8, 65536, 5);
    let (device, key) = enroll_management(&f, vesta_control::identity::BuiltinRole::Owner);
    let token = management_session(&f, &device, &key);
    let bytes = vec![b'x'; 512 * 1024];
    let sha = hex::encode(Sha256::digest(&bytes));
    let begin = management_mutation(
        &f,
        &token,
        "pack/draft/uploads/begin",
        "capacity-begin-0001",
        &json!({"upload_id":"large-chunk","total_size":bytes.len().to_string(),"sha256":sha,"archive":true}),
    );
    assert_eq!(
        begin.status,
        200,
        "{}",
        String::from_utf8_lossy(&begin.body)
    );
    let chunk =
        json!({"upload_id":"large-chunk","offset":"0","content_base64":STANDARD.encode(&bytes)});
    // Send only the oversized declaration: the server rejects it before body I/O.
    // Writing the rejected body itself can race the intentional socket close on Windows.
    let mut anonymous=f.stream();
    write!(anonymous,"POST /api/v3/manage/pack/draft/uploads/chunk HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",serde_json::to_vec(&chunk).unwrap().len()).unwrap();
    anonymous.flush().unwrap();
    assert_eq!(read_reply(anonymous).unwrap().status,401);
    let uploaded = management_mutation(
        &f,
        &token,
        "pack/draft/uploads/chunk",
        "capacity-chunk-0001",
        &chunk,
    );
    assert_eq!(
        uploaded.status,
        200,
        "{}",
        String::from_utf8_lossy(&uploaded.body)
    );
    assert_eq!(uploaded.json()["data"]["size"], bytes.len().to_string());
    let finished = management_mutation(
        &f,
        &token,
        "pack/draft/uploads/finish",
        "capacity-finish-0001",
        &json!({"upload_id":"large-chunk"}),
    );
    assert_eq!(
        finished.status,
        200,
        "{}",
        String::from_utf8_lossy(&finished.body)
    );
    assert_eq!(
        f.request("GET", "/api/v3/manage/server/status", b"", Some(&token))
            .status,
        200
    );
}
