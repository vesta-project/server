//! Real authenticated TLS workspace lifecycle; no provider requests or game launch.
use super::*;
use serde_json::json;
#[test]
fn signed_pack_sync_binds_freshness_identity_policy_and_latest_download() {
    use base64::{Engine,engine::general_purpose::STANDARD};
    let f=Fixture::new("minecraft",8,65536,5);
    let (device,key)=enroll_management(&f,vesta_control::identity::BuiltinRole::Owner);
    let token=management_session(&f,&device,&key);
    let initial=f.request("GET","/api/v3/manage/pack/draft",b"",Some(&token));
    let manifest=initial.json()["data"]["draft"]["manifest"].clone();
    let saved=management_mutation(&f,&token,"pack/draft","sync-edit-00000001",&json!({"expected_revision":"0","manifest":manifest}));assert_eq!(saved.status,200);
    let preview=management_mutation(&f,&token,"pack/preview","sync-preview-0001",&json!({"draft_revision":"1","resolved_entries":[]}));assert_eq!(preview.status,200);
    let published=management_mutation(&f,&token,"pack/publish","sync-publish-0001",&json!({"expected_revision":"0","draft_revision":"1","resolved_entries":[],"review_sha256":preview.json()["data"]["review_sha256"],"required_update":true,"acknowledge_lower_version":false}));assert_eq!(published.status,200);
    let path="/api/v3/manage/pack/sync?installed=0&nonce=0123456789abcdef0123456789abcdef";
    assert_eq!(f.request("GET",path,b"",None).status,401);
    let response=f.request("GET",path,b"",Some(&token));assert_eq!(response.status,200,"{}",String::from_utf8_lossy(&response.body));
    let envelope=response.json()["data"].clone();
    let payload:serde_json::Value=serde_json::from_slice(&STANDARD.decode(envelope["payload_base64"].as_str().unwrap()).unwrap()).unwrap();
    assert_eq!(payload["revision"],"1");assert_eq!(payload["minimum_revision"],"1");assert_eq!(payload["status"],"update_required");
    assert_eq!(payload["server_id"],f.host().management().server_id());
    assert_eq!(payload["nonce"],"0123456789abcdef0123456789abcdef");
    assert!(!STANDARD.decode(envelope["signature_base64"].as_str().unwrap()).unwrap().is_empty());
    assert_eq!(payload["fingerprint"],f.transport.as_ref().unwrap().tls_fingerprint().unwrap());
    let download=format!("/api/v3/manage/pack/artifact?revision=1&sha256={}",payload["sha256"].as_str().unwrap());
    let artifact=f.request("GET",&download,b"",Some(&token));assert_eq!(artifact.status,200);
    assert_eq!(artifact.body.len().to_string(),payload["size"].as_str().unwrap());
    let stale=download.replace("revision=1","revision=0");assert_eq!(f.request("GET",&stale,b"",Some(&token)).status,412);
    assert_eq!(f.request("GET",&path.replace("installed=0","installed=2"),b"",Some(&token)).status,409);
    assert_eq!(f.request("GET",&path.replace("installed=0","installed=01"),b"",Some(&token)).status,400);
    assert_eq!(f.request("POST",path,b"",Some(&token)).status,405);
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
