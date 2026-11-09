//! Real authenticated TLS workspace lifecycle; no provider requests or game launch.
use super::*;
use serde_json::json;
#[path = "lifecycle.rs"]
mod lifecycle;
mod real_transitions;
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

// Run-2 audit: real loopback TLS routes, temporary host/database, no game/provider IO.
fn audit_software_fixture() -> Fixture {
    audit_software_fixture_with_java(None)
}
fn audit_software_fixture_with_java(java: Option<&str>) -> Fixture {
    let mut f=Fixture::new("minecraft",8,65536,30);
    let config=f.host().configuration().clone();
    f.finish();
    std::fs::write(f.temp.path().join("bin/java.exe"),b"never executed fixture").unwrap();
    let executable=java.unwrap_or("../bin/java.exe").to_owned();
    assert!(!executable.contains('\''));
    std::fs::write(f.temp.path().join("private/launch.toml"),format!("[minecraft]\nworking-directory = 'game'\n[launch]\nkind = 'native'\nexecutable = '{executable}'\nstop = 'console-stop'\n")).unwrap();
    let ListenerDescription::Tls(listener)=config.listener() else {panic!("TLS fixture")};
    let listener=listener.on_ephemeral_port();
    let host=Host::open(config.clone(),f.clock.clone()).unwrap();
    let transport=Transport::start(host.network_handle(),&listener).unwrap();
    f.address=transport.local_addr();f.host=Some(host);f.transport=Some(transport);
    f
}
#[test]
fn audit_endpoint_pack_job_lifecycle() {
    use base64::{Engine, engine::general_purpose::STANDARD};
    let mut f = audit_software_fixture();
    // A fixture runtime is already installed. This changes no executable and never launches it.
    f.host().database().write(|c| {
        c.execute("UPDATE software_state SET current_launch=json_set(current_launch,'$.installation.software','vanilla','$.installation.version','1.21.1') WHERE singleton=1",[])?;
        Ok(())
    }).unwrap();
    let (device,key)=enroll_management(&f,vesta_control::identity::BuiltinRole::Owner);
    let mut token=management_session(&f,&device,&key);
    let (viewer,viewer_key)=enroll_management(&f,vesta_control::identity::BuiltinRole::Viewer);
    let viewer=management_session(&f,&viewer,&viewer_key);
    let path="/api/v3/manage/pack/install/plan";
    assert_eq!(f.request("POST",path,b"{}",None).status,401);
    assert_eq!(f.request("POST",path,b"{}",Some(&viewer)).status,403);
    assert_eq!(f.request("POST",path,b"{}",Some(&token)).status,400);
    let mut revision="0".to_owned();
    for i in 0..8 {
        let r=management_mutation(&f,&token,"pack/files/mutate",&format!("audit-edit-{i:08}"),&json!({"expected_revision":revision,"change":{"operation":"write","root":"overrides","path":format!("config/audit-{i}.json"),"content_base64":STANDARD.encode(b"{}\n")}}));
        assert_eq!(r.status,200,"{}",String::from_utf8_lossy(&r.body));
        revision=r.json()["data"]["revision"].as_str().unwrap().into();
    }
    let input=json!({"analyze":false,"source":{"source":"draft","revision":revision}});
    let clock=Instant::now();
    let r=f.request("POST",path,&serde_json::to_vec(&input).unwrap(),Some(&token));
    eprintln!("AUDIT_ENDPOINT plan_ms={:.3} response_bytes={}",clock.elapsed().as_secs_f64()*1000.0,r.body.len());
    assert_eq!(r.status,200,"{}",String::from_utf8_lossy(&r.body));
    let plan=r.json()["data"].clone();
    assert_eq!(plan["valid"],true);
    // A prepared, actor-bound plan must survive a host interruption before Apply.
    audit_reopen_host(&mut f);assert_eq!(audit_long_get(&f,&token,"software/jobs").status,401);
    token=management_session(&f,&device,&key);
    let apply=json!({"plan_id":plan["plan_id"],"confirmation_token":plan["confirmation_token"],"acknowledge_warnings":true,"acknowledge_software_switch":false,"choices":[],"set_as_modpack":false});
    let mut bad=apply.clone();bad["confirmation_token"]=json!("wrong");
    let refused=management_mutation(&f,&token,"pack/install/apply","audit-invalid-0001",&bad);
    assert!(refused.status>=400,"invalid confirmation accepted");
    // Lose the first response after sending the complete mutation. Reconnect with
    // the SAME intent: a dropped HTTP waiter must not become a duplicate install.
    let body=serde_json::to_vec(&apply).unwrap();
    let mut abandoned=f.stream();
    write!(abandoned,"POST /api/v3/manage/pack/install/apply HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\nAuthorization: Bearer {token}\r\nIdempotency-Key: audit-install-0001\r\n\r\n",body.len()).unwrap();
    abandoned.write_all(&body).unwrap();abandoned.flush().unwrap();drop(abandoned);
    let clock=Instant::now();
    let accepted=management_mutation(&f,&token,"pack/install/apply","audit-install-0001",&apply);
    eprintln!("AUDIT_ENDPOINT accept_ms={:.3}",clock.elapsed().as_secs_f64()*1000.0);
    assert_eq!(accepted.status,200,"{}",String::from_utf8_lossy(&accepted.body));
    let job=accepted.json()["data"]["software_job_id"].as_str().unwrap().to_owned();
    let until=Instant::now()+Duration::from_secs(30);
    loop {
        // Each request reconnects over a new TLS connection.
        let r=f.request("GET","/api/v3/manage/software/jobs",b"",Some(&token));
        assert_eq!(r.status,200);
        let value=r.json();
        let row=value["data"]["jobs"].as_array().unwrap().iter().find(|j|j["id"]==job).unwrap();
        let state=row["state"].as_str().unwrap();
        if state=="succeeded" {break;}
        assert!(!matches!(state,"failed"|"recovery_required"),"{row}");
        assert!(Instant::now()<until,"job timeout");
        std::thread::sleep(Duration::from_millis(20));
    }
    eprintln!("AUDIT_ENDPOINT post_install_ms={:.3}",clock.elapsed().as_secs_f64()*1000.0);
    for i in 0..8 {assert_eq!(std::fs::read(f.temp.path().join(format!("game/config/audit-{i}.json"))).unwrap(),b"{}\n");}
    let replay=management_mutation(&f,&token,"pack/install/apply","audit-install-0001",&apply);
    assert_eq!(replay.status,200,"{}",String::from_utf8_lossy(&replay.body));
    assert_eq!(replay.json()["data"]["software_job_id"],job);
    let jobs=f.request("GET","/api/v3/manage/software/jobs",b"",Some(&token));
    assert_eq!(jobs.status,200);
    assert_eq!(jobs.json()["data"]["jobs"].as_array().unwrap().len(),1,"lost-response retry must create exactly one job");
    let linked=f.request("GET","/api/v3/manage/pack/server-link",b"",Some(&token));
    assert_eq!(linked.status,200);
    assert_eq!(linked.json()["data"]["state"],"installed");
    let config=f.host().configuration().clone();
    f.finish();
    let ListenerDescription::Tls(listener)=config.listener() else {panic!("TLS fixture")};
    let listener=listener.on_ephemeral_port();
    let host=Host::open(config.clone(),f.clock.clone()).unwrap();
    let transport=Transport::start(host.network_handle(),&listener).unwrap();
    f.address=transport.local_addr();f.host=Some(host);f.transport=Some(transport);
    assert_eq!(f.request("GET","/api/v3/manage/software/jobs",b"",Some(&token)).status,401);
    let renewed=management_session(&f,&device,&key);
    let persisted=f.request("GET","/api/v3/manage/software/jobs",b"",Some(&renewed));
    assert_eq!(persisted.status,200);
    assert!(persisted.json()["data"]["jobs"].as_array().unwrap().iter().any(|row|row["id"]==job && row["state"]=="succeeded"));
}

#[test]
#[ignore = "bounded real provider metadata via isolated HTTPS endpoint, no apply or game launch"]
fn audit_real_provider_endpoint_plans() {
    for (name,provider,project,version) in [("RLCraft","curseforge","285109","4612979"),("ATM10","curseforge","925200","9431424"),("Cobbleverse","modrinth","Jkb29YJU","4SKGla61")] {
        let f=audit_software_fixture();
        let (device,key)=enroll_management(&f,vesta_control::identity::BuiltinRole::Owner);
        let token=management_session(&f,&device,&key);
        let request=json!({"analyze":false,"source":{"source":"provider","provider":provider,"project_id":project,"version_id":version}});
        for phase in ["cold","warm"] {
            let clock=Instant::now();
            let r=f.request("POST","/api/v3/manage/pack/install/plan",&serde_json::to_vec(&request).unwrap(),Some(&token));
            eprintln!("AUDIT_REAL name={name} provider={provider} project={project} version={version} phase={phase} ms={:.3} status={} bytes={} body={}",clock.elapsed().as_secs_f64()*1000.0,r.status,r.body.len(),String::from_utf8_lossy(&r.body));
            // Refusals are recorded as blockers, never counted as installation success.
            if r.status!=200 {break;}
            assert_eq!(r.json()["data"]["valid"],true);
        }
    }
}

#[test]
#[ignore = "bounded endpoint performance fixture; real TLS temporary host, no external requests"]
fn audit_endpoint_uploaded_pack_baseline() {
    use base64::{Engine,engine::general_purpose::STANDARD};
    use sha2::{Digest,Sha256};
    let f=audit_software_fixture();
    f.host().database().write(|c|{c.execute("UPDATE software_state SET current_launch=json_set(current_launch,'$.installation.software','vanilla','$.installation.version','1.21.1') WHERE singleton=1",[])?;Ok(())}).unwrap();
    let (device,key)=enroll_management(&f,vesta_control::identity::BuiltinRole::Owner);
    let token=management_session(&f,&device,&key);
    let mut zip=zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    for i in 0..100 {zip.start_file(format!("config/endpoint-{i}.json"),zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored)).unwrap();zip.write_all(b"{}\n").unwrap();}
    let bytes=zip.finish().unwrap().into_inner();
    let started=Instant::now();
    let r=management_mutation(&f,&token,"pack/draft/uploads/begin","audit-upload-begin",&json!({"upload_id":"endpoint","total_size":bytes.len().to_string(),"sha256":hex::encode(Sha256::digest(&bytes)),"archive":true}));
    assert_eq!(r.status,200,"{}",String::from_utf8_lossy(&r.body));
    for (i,chunk) in bytes.chunks(16000).enumerate() {let r=management_mutation(&f,&token,"pack/draft/uploads/chunk",&format!("audit-upload-{i:08}"),&json!({"upload_id":"endpoint","offset":(i*16000).to_string(),"content_base64":STANDARD.encode(chunk)}));assert_eq!(r.status,200,"{}",String::from_utf8_lossy(&r.body));}
    let r=management_mutation(&f,&token,"pack/draft/uploads/finish","audit-upload-finish",&json!({"upload_id":"endpoint"}));assert_eq!(r.status,200,"{}",String::from_utf8_lossy(&r.body));
    eprintln!("AUDIT_ENDPOINT100 upload_ms={:.3} bytes={}",started.elapsed().as_secs_f64()*1000.0,bytes.len());
    let started=Instant::now();
    let r=f.request("POST","/api/v3/manage/pack/archive/inspect",br#"{"upload_id":"endpoint"}"#,Some(&token));assert_eq!(r.status,200,"{}",String::from_utf8_lossy(&r.body));
    eprintln!("AUDIT_ENDPOINT100 inspect_ms={:.3}",started.elapsed().as_secs_f64()*1000.0);
    let started=Instant::now();
    let input=json!({"analyze":false,"source":{"source":"uploaded_archive","upload_id":"endpoint"}});
    // This benchmark admits the same 30s request deadline as its dedicated host.
    // The unrelated existing smoke-fixture 3s socket budget remains unchanged.
    let body=serde_json::to_vec(&input).unwrap();
    let mut stream=f.stream();stream.sock.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
    write!(stream,"POST /api/v3/manage/pack/install/plan HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\nAuthorization: Bearer {token}\r\n\r\n",body.len()).unwrap();
    stream.write_all(&body).unwrap();stream.flush().unwrap();
    let r=read_reply(stream).unwrap();assert_eq!(r.status,200,"{}",String::from_utf8_lossy(&r.body));
    eprintln!("AUDIT_ENDPOINT100 plan_ms={:.3} response_bytes={}",started.elapsed().as_secs_f64()*1000.0,r.body.len());
    let plan=r.json()["data"].clone();assert_eq!(plan["files"].as_array().unwrap().len(),100);
    let started=Instant::now();
    let r=management_mutation(&f,&token,"pack/install/apply","audit-upload-apply",&json!({"plan_id":plan["plan_id"],"confirmation_token":plan["confirmation_token"],"acknowledge_warnings":true,"acknowledge_software_switch":false,"choices":[],"set_as_modpack":false}));assert_eq!(r.status,200,"{}",String::from_utf8_lossy(&r.body));
    eprintln!("AUDIT_ENDPOINT100 acceptance_ms={:.3}",started.elapsed().as_secs_f64()*1000.0);
    let job=r.json()["data"]["software_job_id"].as_str().unwrap().to_owned();
    let until=Instant::now()+Duration::from_secs(180);
    loop {let r=f.request("GET","/api/v3/manage/software/jobs",b"",Some(&token));assert_eq!(r.status,200);let value=r.json();let row=value["data"]["jobs"].as_array().unwrap().iter().find(|j|j["id"]==job).unwrap();if row["state"]=="succeeded" {break;}assert_ne!(row["state"],"failed","{row}");assert!(Instant::now()<until,"endpoint job timeout");std::thread::sleep(Duration::from_millis(100));}
    eprintln!("AUDIT_ENDPOINT100 post_install_ms={:.3}",started.elapsed().as_secs_f64()*1000.0);
    for i in 0..100 {assert_eq!(std::fs::read(f.temp.path().join(format!("game/config/endpoint-{i}.json"))).unwrap(),b"{}\n");}
    for phase in ["cold","warm"] {let start=Instant::now();let r=f.request("GET","/api/v3/manage/pack/draft",b"",Some(&token));assert_eq!(r.status,200);eprintln!("AUDIT_ENDPOINT100 reopen={phase} ms={:.3} bytes={}",start.elapsed().as_secs_f64()*1000.0,r.body.len());}
}

fn audit_long_post(f:&Fixture,token:&str,action:&str,key:Option<&str>,input:&serde_json::Value)->WireReply {
    let body=serde_json::to_vec(input).unwrap();let mut stream=f.stream();stream.sock.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
    let intent=key.map(|key|format!("Idempotency-Key: {key}\r\n")).unwrap_or_default();
    write!(stream,"POST /api/v3/manage/{action} HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\nAuthorization: Bearer {token}\r\n{intent}\r\n",body.len()).unwrap();
    stream.write_all(&body).unwrap();stream.flush().unwrap();read_reply(stream).unwrap()
}
#[test]
#[ignore = "real pinned Modrinth pack/runtime downloads on temporary HTTP host; explicit Java required; no game start"]
fn audit_real_cobbleverse_endpoint_install() {
    let java=std::env::var("VESTA_AUDIT_REAL_JAVA").expect("explicit safe Java path required");
    assert!(std::path::Path::new(&java).is_absolute());
    let f=audit_software_fixture_with_java(Some(&java));
    let (device,key)=enroll_management(&f,vesta_control::identity::BuiltinRole::Owner);
    let token=management_session(&f,&device,&key);
    let input=json!({"analyze":false,"source":{"source":"provider","provider":"modrinth","project_id":"Jkb29YJU","version_id":"4SKGla61"}});
    let started=Instant::now();
    let r=audit_long_post(&f,&token,"pack/install/plan",None,&input);
    eprintln!("AUDIT_REAL_INSTALL plan_ms={:.3} status={}",started.elapsed().as_secs_f64()*1000.0,r.status);
    assert_eq!(r.status,200,"{}",String::from_utf8_lossy(&r.body));
    let plan=r.json()["data"].clone();assert_eq!(plan["software"],"fabric");assert_eq!(plan["minecraft_version"],"1.21.1");assert_eq!(plan["loader_version"],"0.18.4");
    let apply=json!({"plan_id":plan["plan_id"],"confirmation_token":plan["confirmation_token"],"acknowledge_warnings":true,"acknowledge_software_switch":true,"choices":[],"set_as_modpack":true});
    let started=Instant::now();let r=audit_long_post(&f,&token,"pack/install/apply",Some("real-cobbleverse-audit-01"),&apply);
    eprintln!("AUDIT_REAL_INSTALL acceptance_ms={:.3} status={} body={}",started.elapsed().as_secs_f64()*1000.0,r.status,String::from_utf8_lossy(&r.body));
    assert_eq!(r.status,200,"{}",String::from_utf8_lossy(&r.body));
    let job=r.json()["data"]["software_job_id"].as_str().unwrap().to_owned();
    let until=Instant::now()+Duration::from_secs(900);let mut last=String::new();let mut polls=0;
    loop {
        let r=f.request("GET","/api/v3/manage/software/jobs",b"",Some(&token));assert_eq!(r.status,200);let value=r.json();let row=value["data"]["jobs"].as_array().unwrap().iter().find(|j|j["id"]==job).unwrap();let state=row["state"].as_str().unwrap();
        if state!=last {eprintln!("AUDIT_REAL_INSTALL state={state} elapsed_ms={:.3} row={row}",started.elapsed().as_secs_f64()*1000.0);last=state.into();}
        if state=="succeeded" {break;}
        assert!(!matches!(state,"failed"|"recovery_required"),"real endpoint job refused: {row}");assert!(Instant::now()<until,"bounded real install deadline reached");
        if polls%5==0 {let r=f.request("GET","/api/v3/manage/pack/draft",b"",Some(&token));eprintln!("AUDIT_REAL_INSTALL progress_status={} progress={}",r.status,r.json()["data"]["install_progress"]);}
        polls+=1;std::thread::sleep(Duration::from_secs(1));
    }
    eprintln!("AUDIT_REAL_INSTALL complete_ms={:.3} polls={polls}",started.elapsed().as_secs_f64()*1000.0);
    let replay=audit_long_post(&f,&token,"pack/install/apply",Some("real-cobbleverse-audit-01"),&apply);assert_eq!(replay.status,200);assert_eq!(replay.json()["data"]["software_job_id"],job);
    for phase in ["cold","warm"] {let clock=Instant::now();let r=f.request("GET","/api/v3/manage/pack/server-link",b"",Some(&token));assert_eq!(r.status,200);assert_eq!(r.json()["data"]["state"],"installed");eprintln!("AUDIT_REAL_INSTALL reopen={phase} ms={:.3} bytes={}",clock.elapsed().as_secs_f64()*1000.0,r.body.len());}
    assert!(std::fs::read_dir(f.temp.path().join("game/mods")).unwrap().count()>0);
}

#[test]
#[ignore = "bounded real-provider renamed-entry validation; explicitly run with pinned local index, no payload install"]
fn audit_real_renamed_entry_endpoint_repro() {
    use base64::{Engine,engine::general_purpose::STANDARD};use sha2::{Digest,Sha256};
    let original:serde_json::Value=serde_json::from_slice(&std::fs::read(std::env::var("VESTA_AUDIT_MRPACK_INDEX").expect("pinned local index required")).unwrap()).unwrap();
    let row=original["files"].as_array().unwrap().iter().find(|r|r["path"]=="mods/cobblecuisine-2.0.1-1.7.rc1.jar").unwrap().clone();
    let manifest=json!({"formatVersion":1,"game":"minecraft","versionId":"audit-pinned","name":"Renamed real entry","dependencies":original["dependencies"],"files":[row]});
    let mut zip=zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));zip.start_file("modrinth.index.json",zip::write::SimpleFileOptions::default()).unwrap();zip.write_all(&serde_json::to_vec(&manifest).unwrap()).unwrap();let bytes=zip.finish().unwrap().into_inner();
    let f=audit_software_fixture();let (device,key)=enroll_management(&f,vesta_control::identity::BuiltinRole::Owner);let token=management_session(&f,&device,&key);
    for(action,key,body)in [("pack/draft/uploads/begin","renamed-begin-0001",json!({"upload_id":"renamed","total_size":bytes.len().to_string(),"sha256":hex::encode(Sha256::digest(&bytes)),"archive":true})),("pack/draft/uploads/chunk","renamed-chunk-0001",json!({"upload_id":"renamed","offset":"0","content_base64":STANDARD.encode(&bytes)})),("pack/draft/uploads/finish","renamed-finish-001",json!({"upload_id":"renamed"}))] {let r=management_mutation(&f,&token,action,key,&body);assert_eq!(r.status,200,"{}",String::from_utf8_lossy(&r.body));}
    let r=audit_long_post(&f,&token,"pack/install/plan",None,&json!({"analyze":false,"source":{"source":"uploaded_archive","upload_id":"renamed"}}));
    eprintln!("AUDIT_RENAMED_ENDPOINT status={} body={}",r.status,String::from_utf8_lossy(&r.body));
    assert_eq!(r.status,200,"valid pinned mrpack reference with equal provider hash must retain its safe requested filename");
    assert_eq!(r.json()["data"]["files"][0]["path"],"mods/cobblecuisine-2.0.1-1.7.rc1.jar");
    let plan_id=r.json()["data"]["plan_id"].as_str().unwrap().to_owned();
    let stored:String=f.host().database().read(|c|Ok(c.query_row("SELECT body FROM pack_install_plans WHERE id=?",[plan_id],|r|r.get(0))?)).unwrap();let stored:serde_json::Value=serde_json::from_str(&stored).unwrap();
    assert_eq!(stored["artifact_pins"][0]["reference"]["project_id"],"dlx62Wp3");
    assert_eq!(stored["artifact_pins"][0]["sha512"],manifest["files"][0]["hashes"]["sha512"]);
    assert_eq!(stored["artifact_pins"][0]["filename"],"cobblecuisine-2.0.1-1.7-rc1.jar");
    let mut draft=audit_long_get(&f,&token,"pack/draft").json()["data"]["draft"]["manifest"].clone();draft["loader"]="fabric".into();draft["loader_version"]="0.18.4".into();
    draft["entries"]=json!([{"id":"mismatched-artifact","kind":"mod","path":"mods/renamed.jar","artifact_sha512":"f".repeat(128),"required":true,"environment":{"client":"required","server":"required"},"source":{"type":"provider","provider":"modrinth","project_id":"dlx62Wp3","version_id":"KQwSroDP"}}]);
    let written=management_mutation(&f,&token,"pack/draft","mismatch-draft-01",&json!({"expected_revision":"0","manifest":draft}));assert_eq!(written.status,200,"{}",written.json());
    let denied=audit_long_post(&f,&token,"pack/install/plan",None,&json!({"analyze":false,"source":{"source":"draft","revision":written.json()["data"]["revision"]}}));
    assert!(denied.status>=400);assert_eq!(denied.json()["error"]["details"]["stage"],"artifact_binding");assert_eq!(denied.json()["error"]["details"]["projectId"],"dlx62Wp3");
    assert_eq!(audit_long_get(&f,&token,"software/jobs").json()["data"]["jobs"].as_array().unwrap().len(),0);
}

fn audit_reopen_host(f:&mut Fixture) {
    let config=f.host().configuration().clone();f.finish();
    let ListenerDescription::Tls(listener)=config.listener() else {panic!("TLS fixture")};let listener=listener.on_ephemeral_port();
    let host=Host::open(config.clone(),f.clock.clone()).unwrap();let transport=Transport::start(host.network_handle(),&listener).unwrap();f.address=transport.local_addr();f.host=Some(host);f.transport=Some(transport);
}
fn audit_long_get(f:&Fixture,token:&str,action:&str)->WireReply {
    let mut stream=f.stream();stream.sock.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
    write!(stream,"GET /api/v3/manage/{action} HTTP/1.1\r\nHost: localhost\r\nContent-Length: 0\r\nConnection: close\r\nAuthorization: Bearer {token}\r\n\r\n").unwrap();stream.flush().unwrap();read_reply(stream).unwrap()
}
fn audit_wait_job(f:&Fixture,token:&mut String,device:&str,key:&ring::signature::Ed25519KeyPair,id:&str,seconds:u64)->Result<serde_json::Value,serde_json::Value> {
    let started=Instant::now();let until=started+Duration::from_secs(seconds);let mut last=String::new();let mut tick=0;
    loop {
        let r=audit_long_get(f,token,"software/jobs");
        if r.status==401 {*token=management_session(f,device,key);continue;}
        assert_eq!(r.status,200,"job read failed status={}",r.status);
        let value=r.json();let row=value["data"]["jobs"].as_array().unwrap().iter().find(|row|row["id"]==id).unwrap().clone();let state=row["state"].as_str().unwrap();
        if state!=last {eprintln!("AUDIT_JOB elapsed_ms={:.3} row={row}",started.elapsed().as_secs_f64()*1000.0);last=state.into();}
        if state=="succeeded" {return Ok(row)}
        if matches!(state,"failed"|"recovery_required") {return Err(row)}
        if tick%5==0 && row["kind"]=="apply" {let r=audit_long_get(f,token,"pack/draft");if r.status==401 {*token=management_session(f,device,key);continue;}assert_eq!(r.status,200);eprintln!("AUDIT_PROGRESS elapsed_ms={:.3} progress={}",started.elapsed().as_secs_f64()*1000.0,r.json()["data"]["install_progress"]);}
        assert!(Instant::now()<until,"bounded job timeout");tick+=1;std::thread::sleep(Duration::from_secs(1));
    }
}
#[test]
fn audit_endpoint_failure_context_survives_restart_and_requires_authority() {
    let mut f=audit_software_fixture();let (device,key)=enroll_management(&f,vesta_control::identity::BuiltinRole::Owner);let token=management_session(&f,&device,&key);
    assert_eq!(audit_long_get(&f,&token,"pack/draft").status,200);
    let subject=f.host().management().authorize(&token,vesta_control::identity::Capability::ContentInstall,None).unwrap();
    let context=json!({"provider":"modrinth","project_id":"project","version_id":"version","filename":"resource.jar","phase":"preparation","stage":"download_verify","code":"content_pack_hash_mismatch"});
    f.host().database().write(|c|{c.execute("INSERT INTO software_jobs(id,kind,state,principal,device,idempotency_key,request_hash,expected_revision,created_at_ms,updated_at_ms,error) VALUES('failure-context','apply','failed',?,?,'context-intent','binding',0,0,0,'content_pack_hash_mismatch')",rusqlite::params![subject.principal.as_str(),subject.device.as_str()])?;c.execute("INSERT INTO software_job_failures VALUES('failure-context',?)",[context.to_string()])?;c.execute("INSERT INTO pack_install_job VALUES(?,'failure-context')",[f.host().database().server_id()])?;Ok(())}).unwrap();
    let r=audit_long_get(&f,&token,"software/jobs");assert_eq!(r.status,200);assert_eq!(r.json()["data"]["jobs"][0]["error_context"],context);
    let workspace=audit_long_get(&f,&token,"pack/draft");assert_eq!(workspace.status,200);assert_eq!(workspace.json()["data"]["install_job"]["error_context"],context);
    let (viewer,k)=enroll_management(&f,vesta_control::identity::BuiltinRole::Viewer);let viewer=management_session(&f,&viewer,&k);assert_eq!(audit_long_get(&f,&viewer,"software/jobs").status,403);assert_eq!(audit_long_get(&f,&viewer,"pack/draft").status,403);
    audit_reopen_host(&mut f);assert_eq!(audit_long_get(&f,&token,"software/jobs").status,401);let renewed=management_session(&f,&device,&key);assert_eq!(audit_long_get(&f,&renewed,"software/jobs").json()["data"]["jobs"][0]["error_context"],context);
}
#[test]
#[ignore = "portable managed JRE downloads via isolated authenticated endpoint; no system installation or game launch"]
fn audit_prepare_portable_runtimes() {
    let bootstrap=std::env::var("VESTA_AUDIT_REAL_JAVA").expect("explicit bootstrap Java");let mut f=audit_software_fixture_with_java(Some(&bootstrap));
    let (device,key)=enroll_management(&f,vesta_control::identity::BuiltinRole::Owner);let mut token=management_session(&f,&device,&key);let mut runtimes=Vec::new();
    for major in [8,21] {
        let r=audit_long_post(&f,&token,"java/download",Some(&format!("audit-managed-java-{major}")),&json!({"major":major}));
        eprintln!("AUDIT_JAVA major={major} accepted_status={}",r.status);if r.status!=202 {eprintln!("AUDIT_JAVA refusal={}",r.json());continue;}
        let id=r.json()["data"]["id"].as_str().unwrap().to_owned();if let Err(row)=audit_wait_job(&f,&mut token,&device,&key,&id,300) {eprintln!("AUDIT_JAVA blocked={row}");continue;}
        let inventory=audit_long_get(&f,&token,"java");assert_eq!(inventory.status,200);let value=inventory.json();let runtime=value["data"]["runtimes"].as_array().unwrap().iter().find(|r|r["major"]==major && r["managed"]==true).unwrap().clone();runtimes.push(runtime);
    }
    let output=std::env::var("VESTA_AUDIT_RUNTIME_CACHE").expect("scratch runtime record path");std::fs::write(output,serde_json::to_vec_pretty(&runtimes).unwrap()).unwrap();
    f.finish();let owned=std::mem::replace(&mut f.temp,tempfile::tempdir().unwrap());let retained=owned.keep();eprintln!("AUDIT_JAVA retained_fixture={} available={}",retained.display(),runtimes.len());assert_eq!(runtimes.len(),2,"managed runtime setup incomplete; preserve precise per-major failures");
}

#[test]
#[ignore = "real pinned provider pack on isolated authenticated host; managed runtime required; never launches Minecraft"]
fn audit_pinned_pack_endpoint_install() {
    use sha2::{Digest,Sha256};
    let case=std::env::var("VESTA_AUDIT_PACK").expect("explicit pack case");
    let (provider,project,version,mc,loader,loader_version,major)=match case.as_str(){"rlcraft"=>("curseforge","285109","4612979","1.12.2","forge","14.23.5.2860",8),"atm10"=>("curseforge","925200","9431424","1.21.1","neoforge","21.1.265",21),"cobbleverse"=>("modrinth","Jkb29YJU","4SKGla61","1.21.1","fabric","0.18.4",21),_=>panic!("unsupported audit case")};
    let runtimes:Vec<serde_json::Value>=serde_json::from_slice(&std::fs::read(std::env::var("VESTA_AUDIT_RUNTIME_CACHE").expect("managed runtime record")).unwrap()).unwrap();
    let runtime=runtimes.iter().find(|r|r["major"]==major).expect("compatible portable runtime unavailable; inspect runtime setup log");
    let mut f=audit_software_fixture_with_java(Some(runtime["path"].as_str().unwrap()));let base=f.host().configuration().base().to_owned();
    // Keep the owned installation even if a later assertion fails while Host closes.
    let owned=std::mem::replace(&mut f.temp,tempfile::tempdir().unwrap());let retained=owned.keep();eprintln!("AUDIT_MATRIX retained_root={}",retained.display());
    // Managed documents must survive a pack which ships its own copies.
    let properties=b"motd=installation-audit-preserved\n";std::fs::write(base.join("game/server.properties"),properties).unwrap();
    let (device,key)=enroll_management(&f,vesta_control::identity::BuiltinRole::Owner);let mut token=management_session(&f,&device,&key);
    let input=json!({"analyze":false,"source":{"source":"provider","provider":provider,"project_id":project,"version_id":version}});
    let clock=Instant::now();let r=audit_long_post(&f,&token,"pack/install/plan",None,&input);eprintln!("AUDIT_MATRIX case={case} phase=plan ms={:.3} status={}",clock.elapsed().as_secs_f64()*1000.0,r.status);assert_eq!(r.status,200,"{}",String::from_utf8_lossy(&r.body));
    let plan=r.json()["data"].clone();assert_eq!(plan["minecraft_version"],mc);assert_eq!(plan["software"],loader);assert_eq!(plan["loader_version"],loader_version);
    let mut apply=json!({"plan_id":plan["plan_id"],"confirmation_token":plan["confirmation_token"],"acknowledge_warnings":true,"acknowledge_software_switch":true,"choices":[],"set_as_modpack":true});
    let mut bad=apply.clone();bad["confirmation_token"]="tampered".into();let denied=audit_long_post(&f,&token,"pack/install/apply",Some("matrix-invalid-confirmation"),&bad);assert!(denied.status>=400);assert_eq!(audit_long_get(&f,&token,"software/jobs").json()["data"]["jobs"].as_array().unwrap().len(),0);
    let clock=Instant::now();let r=audit_long_post(&f,&token,"pack/install/apply",Some("matrix-install-0001"),&apply);eprintln!("AUDIT_MATRIX case={case} phase=accept ms={:.3} status={} body={}",clock.elapsed().as_secs_f64()*1000.0,r.status,r.json());assert_eq!(r.status,200,"{}",String::from_utf8_lossy(&r.body));let id=r.json()["data"]["software_job_id"].as_str().unwrap().to_owned();
    let completed=audit_wait_job(&f,&mut token,&device,&key,&id,1800);
    if let Err(row)=&completed {eprintln!("AUDIT_MATRIX case={case} FAILED={row}");f.finish();eprintln!("AUDIT_MATRIX retained_failure_root={}",retained.display());}
    completed.expect("real provider installation failed; preserve structured context");eprintln!("AUDIT_MATRIX case={case} phase=completion ms={:.3}",clock.elapsed().as_secs_f64()*1000.0);token=management_session(&f,&device,&key);
    let replay=audit_long_post(&f,&token,"pack/install/apply",Some("matrix-install-0001"),&apply);assert_eq!(replay.status,200);assert_eq!(replay.json()["data"]["software_job_id"],id);
    let software=audit_long_get(&f,&token,"software");assert_eq!(software.status,200);assert_eq!(software.json()["data"]["current"]["version"],mc);assert_eq!(software.json()["data"]["current"]["loader_version"],loader_version);assert_eq!(software.json()["data"]["current"]["software"],loader);
    let java=audit_long_get(&f,&token,"java");assert_eq!(java.status,200);assert_eq!(java.json()["data"]["selected"]["major"],major);
    let link=audit_long_get(&f,&token,"pack/server-link");assert_eq!(link.status,200);let link=link.json()["data"].clone();assert_eq!(link["state"],"installed");assert!(link["pending"].is_null());assert_eq!(link["installed"]["source"],input["source"]);
    let mut verified=0usize;for(path,hash)in link["files"].as_object().unwrap(){assert!(!path.to_ascii_lowercase().starts_with("resourcepacks/"));assert!(!path.to_ascii_lowercase().starts_with("shaderpacks/"));let mut file=std::fs::File::open(base.join("game").join(path)).unwrap();let mut digest=Sha256::new();let mut bytes=[0u8;65536];loop{let n=file.read(&mut bytes).unwrap();if n==0{break;}digest.update(&bytes[..n]);}assert_eq!(hex::encode(digest.finalize()),hash.as_str().unwrap(),"receipt/file mismatch: {path}");verified+=1;}
    assert!(verified>0);assert_eq!(std::fs::read(base.join("game/server.properties")).unwrap(),properties);
    let raw:String=f.host().database().read(|c|Ok(c.query_row("SELECT current_launch FROM software_state WHERE singleton=1",[],|r|r.get(0))?)).unwrap();let launch:serde_json::Value=serde_json::from_str(&raw).unwrap();let artifacts:Vec<vesta_server_install::ManagedArtifact>=serde_json::from_value(launch["artifacts"].clone()).unwrap();assert!(!artifacts.is_empty());tokio::runtime::Runtime::new().unwrap().block_on(vesta_server_install::verify_managed_artifacts(&artifacts)).unwrap();
    let pending:i64=f.host().database().read(|c|Ok(c.query_row("SELECT count(*) FROM server_content_changes WHERE state='pending'",[],|r|r.get(0))?)).unwrap();assert_eq!(pending,0);
    let start=Instant::now();let content=audit_long_get(&f,&token,"content");assert_eq!(content.status,200);let data=content.json();let items=data["data"]["items"].as_array().unwrap();eprintln!("AUDIT_MATRIX case={case} phase=inventory ms={:.3} items={} declared={} provider_verified={} verified_receipt_files={} runtime_artifacts={}",start.elapsed().as_secs_f64()*1000.0,items.len(),items.iter().filter(|v|!v["declared"].is_null()).count(),items.iter().filter(|v|!v["provider"].is_null()).count(),verified,artifacts.len());
    audit_reopen_host(&mut f);assert_eq!(audit_long_get(&f,&token,"software/jobs").status,401);token=management_session(&f,&device,&key);for phase in ["cold","warm"]{let start=Instant::now();let r=audit_long_get(&f,&token,"pack/server-link");assert_eq!(r.status,200);assert_eq!(r.json()["data"]["state"],"installed");eprintln!("AUDIT_MATRIX case={case} phase=reopen_{phase} ms={:.3}",start.elapsed().as_secs_f64()*1000.0);}
    let persisted=audit_long_get(&f,&token,"software/jobs");assert!(persisted.json()["data"]["jobs"].as_array().unwrap().iter().any(|r|r["id"]==id&&r["state"]=="succeeded"));
    eprintln!("AUDIT_MATRIX case={case} VERIFIED no_game_launch=true");apply["confirmation_token"]=serde_json::Value::Null;
    if let Ok(output)=std::env::var("VESTA_AUDIT_RESULT_DIR") {
        let output=std::path::PathBuf::from(output);std::fs::create_dir_all(&output).unwrap();
        std::fs::write(output.join(format!("{case}-inventory.json")),serde_json::to_vec_pretty(&data).unwrap()).unwrap();
        std::fs::write(output.join(format!("{case}-receipt.json")),serde_json::to_vec_pretty(&link).unwrap()).unwrap();
    }
    f.finish();eprintln!("AUDIT_MATRIX retained_success_root={}",retained.display());
}

#[test]
#[ignore = "reopens only a retained owned audit fixture; measures inventory without reinstalling or launching the game"]
fn audit_retained_pack_inventory() {
    let root=std::path::PathBuf::from(std::env::var("VESTA_AUDIT_RETAINED_ROOT").expect("explicit retained audit fixture"));
    let root=root.canonicalize().unwrap();let temporary=std::env::temp_dir().canonicalize().unwrap();
    assert!(root.starts_with(&temporary));assert!(root.file_name().unwrap().to_string_lossy().starts_with(".tmp"));
    assert_eq!(std::fs::read(root.join("game/server.properties")).unwrap(),b"motd=installation-audit-preserved\n");
    let document=std::fs::read(root.join("host.toml")).unwrap();
    let config=HostConfiguration::parse_toml(&document,&root,Path::new("host.toml")).unwrap();
    let ListenerDescription::Tls(listener)=config.listener() else {panic!("TLS fixture")};let listener=listener.on_ephemeral_port();
    let clock=Arc::new(FixtureClock::default());let host=Host::open(config,clock.clone()).unwrap();
    let transport=Transport::start(host.network_handle(),&listener).unwrap();let address=transport.local_addr();assert!(address.ip().is_loopback());
    // The dummy TempDir belongs to the wrapper only; retained installation stays intact.
    let mut f=Fixture{temp:tempfile::tempdir().unwrap(),host:Some(host),transport:Some(transport),address,clock,password_observation:std::sync::Mutex::new(None)};
    let (device,key)=enroll_management(&f,vesta_control::identity::BuiltinRole::Owner);let token=management_session(&f,&device,&key);
    for run in 0..3 {let start=Instant::now();let response=audit_long_get(&f,&token,"content");assert_eq!(response.status,200);let data=response.json();let items=data["data"]["items"].as_array().unwrap();
        eprintln!("AUDIT_RETAINED inventory_run={run} ms={:.3} response_bytes={} items={} declared={} provider_verified={}",start.elapsed().as_secs_f64()*1000.0,response.body.len(),items.len(),items.iter().filter(|r|!r["declared"].is_null()).count(),items.iter().filter(|r|!r["provider"].is_null()).count());
        if let Ok(output)=std::env::var("VESTA_AUDIT_RESULT_DIR"){std::fs::create_dir_all(&output).unwrap();std::fs::write(std::path::Path::new(&output).join(format!("retained-inventory-{run}.json")),serde_json::to_vec_pretty(&data).unwrap()).unwrap();}
    }
    let plans:Vec<String>=f.host().database().read(|c|{let mut query=c.prepare("SELECT body FROM pack_install_plans")?;let rows=query.query_map([],|r|r.get(0))?;Ok(rows.collect::<std::result::Result<Vec<_>,_>>()?)}).unwrap();
    for raw in plans {let body:serde_json::Value=serde_json::from_str(&raw).unwrap();if let Some(warnings)=body["plan"]["warnings"].as_array(){for warning in warnings {if warning["code"]=="curseforge_server_pack"{eprintln!("AUDIT_RETAINED server_selection={warning}");}}}}
    f.finish();
}
