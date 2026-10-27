use super::*;
use base64::{Engine, engine::general_purpose::STANDARD};
use sha2::{Digest, Sha256};

fn pack(text: &str) -> Vec<u8> {
    let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    for (path, body) in [
        (
            "pack.mcmeta",
            r#"{"pack":{"pack_format":34,"description":"Hosting test"}}"#,
        ),
        ("assets/fixture/lang/en_us.json", text),
    ] {
        zip.start_file(path, zip::write::SimpleFileOptions::default())
            .unwrap();
        zip.write_all(body.as_bytes()).unwrap();
    }
    zip.finish().unwrap().into_inner()
}
fn mutate(
    f: &Fixture,
    token: &str,
    path: &str,
    key: &str,
    body: serde_json::Value,
) -> serde_json::Value {
    let reply = management_mutation(f, token, &format!("resource-packs/{path}"), key, &body);
    assert_eq!(
        reply.status,
        200,
        "{}",
        String::from_utf8_lossy(&reply.body)
    );
    reply.json()["data"].clone()
}
fn upload(f: &Fixture, token: &str, key: &str, body: &[u8]) -> String {
    let sha = hex::encode(Sha256::digest(body));
    let begin = mutate(
        f,
        token,
        "uploads/begin",
        &format!("{key}-begin-0000001"),
        serde_json::json!({"size":body.len().to_string(),"sha256":sha}),
    );
    mutate(
        f,
        token,
        "uploads/chunk",
        &format!("{key}-chunk-0000001"),
        serde_json::json!({"upload_id":begin["id"],"offset":"0","content_base64":STANDARD.encode(body)}),
    );
    mutate(
        f,
        token,
        "uploads/finish",
        &format!("{key}-finish-0000001"),
        serde_json::json!({"upload_id":begin["id"]}),
    );
    sha
}
#[test]
fn hosted_resource_packs_build_apply_and_public_download_use_real_tls_and_file_writer() {
    let f = Fixture::new("minecraft", 8, 65536, 5);
    // Only this fixture's file is changed. No launch/start/EULA/provider calls.
    let properties = f.temp.path().join("game/server.properties");
    std::fs::write(
        &properties,
        b"# preserve comment\nonline-mode=false\nserver-port=25565\nmotd=Keep this\n",
    )
    .unwrap();
    let (device, key) = enroll_management(&f, vesta_control::identity::BuiltinRole::Owner);
    let token = management_session(&f, &device, &key);
    let initial = f.request("GET", "/api/v3/manage/resource-packs", b"", Some(&token));
    assert_eq!(initial.status, 200);
    assert!(initial.json()["data"]["active"].is_null());
    let no_pending = management_mutation(
        &f,
        &token,
        "resource-packs/recovery/abandon",
        "abandon-no-pending-0001",
        &serde_json::json!({
            "expected_properties_sha256":hex::encode(Sha256::digest(std::fs::read(&properties).unwrap())),"confirm_abandon":true
        }),
    );
    assert_eq!(no_pending.status, 412);
    assert_eq!(
        f.request("GET", "/api/v3/manage/resource-packs", b"", None)
            .status,
        401
    );
    let sha = upload(&f, &token, "pack", &pack(r#"{"hello":"world"}"#));
    let draft = mutate(
        &f,
        &token,
        "draft",
        "draft-save-0000001",
        serde_json::json!({"expected_revision":"0","settings":{
            "public_origin":"https://packs.example.invalid","minecraft_version":"1.21.1","format":{"major":34,"minor":0},
            "description":"Test pack","required":true,"prompt":"Server textures","layers":[{"id":"base","source":{"type":"upload","sha256":sha}}]
        }}),
    );
    assert_eq!(draft["revision"], "1");
    let queued = mutate(
        &f,
        &token,
        "build",
        "build-start-000001",
        serde_json::json!({"expected_revision":"1"}),
    );
    let until = Instant::now() + Duration::from_secs(10);
    let release = loop {
        let status = f.request("GET", "/api/v3/manage/resource-packs", b"", Some(&token));
        assert_eq!(status.status, 200);
        let job = status.json()["data"]["jobs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|j| j["id"] == queued["id"])
            .unwrap()
            .clone();
        if job["state"] == "completed" {
            break job["release"].clone();
        }
        assert_ne!(job["state"], "failed", "{job}");
        assert!(Instant::now() < until);
        std::thread::sleep(Duration::from_millis(30));
    };
    assert!(
        !std::fs::read_to_string(&properties)
            .unwrap()
            .contains("resource-pack=")
    );
    let private_path = format!(
        "/resource-packs/{}.zip",
        release["artifact"]["sha256"].as_str().unwrap()
    );
    assert_eq!(f.request("GET", &private_path, b"", None).status, 404);
    let plan = mutate(
        &f,
        &token,
        "plan",
        "plan-create-000001",
        serde_json::json!({"release_id":release["id"],"expected_revision":"1"}),
    );
    let applied = mutate(
        &f,
        &token,
        "apply",
        "apply-release-0001",
        serde_json::json!({"plan_id":plan["plan_id"],"confirmation_token":plan["confirmation_token"],"acknowledge_external_pack":false}),
    );
    assert_eq!(applied["restart_needed"], true);
    let actual = std::fs::read_to_string(&properties).unwrap();
    assert!(actual.contains("# preserve comment"));
    assert!(actual.contains("motd=Keep this"));
    assert!(actual.contains(release["artifact"]["sha1"].as_str().unwrap()));
    assert!(actual.contains("require-resource-pack=true"));
    assert!(actual.contains("resource-pack-id="));
    let path = format!(
        "/resource-packs/{}.zip",
        release["artifact"]["sha256"].as_str().unwrap()
    );
    let public = f.request("GET", &path, b"", None);
    assert_eq!(public.status, 200);
    assert_eq!(
        hex::encode(Sha256::digest(&public.body)),
        release["artifact"]["sha256"]
    );
    assert_eq!(public.headers["content-type"], "application/zip");
    assert!(public.headers["cache-control"].contains("immutable"));
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(public.body)).unwrap();
    let mut lang = String::new();
    zip.by_name("assets/fixture/lang/en_us.json")
        .unwrap()
        .read_to_string(&mut lang)
        .unwrap();
    assert!(lang.contains("world"));
    assert_eq!(
        f.request("GET", &format!("{path}?token=no"), b"", None)
            .status,
        400
    );
    assert_eq!(
        f.request("GET", "/resource-packs/../../server.properties", b"", None)
            .status,
        404
    );
    assert_eq!(
        f.request("GET", &format!("/resource-packs/{sha}.zip"), b"", None)
            .status,
        404
    );
    assert!(
        f.host()
            .runtime()
            .network_handle()
            .game_snapshot()
            .unwrap()
            .generation
            .is_none()
    );
}

/// Opt-in manual curl/client exercise, localhost and disposable state only. The
/// generated receipt contains short-lived fixture credentials, not user secrets.
#[test]
#[ignore = "interactive localhost laboratory; run explicitly for a manual API exercise"]
fn hosted_resource_pack_manual_lab() {
    let f = Fixture::new("minecraft", 16, 1_048_576, 10);
    std::fs::write(
        f.temp.path().join("game/server.properties"),
        b"# hosted resource pack laboratory\nonline-mode=false\nserver-port=25565\nmotd=Lab\n",
    )
    .unwrap();
    let (device, key) = enroll_management(&f, vesta_control::identity::BuiltinRole::Owner);
    let token = management_session(&f, &device, &key);
    let path = f.temp.path().join("hosted-pack-lab.json");
    let ca = f.temp.path().join("test-ca.pem");
    std::fs::write(&ca, CA).unwrap();
    std::fs::write(&path,serde_json::to_vec_pretty(&serde_json::json!({
        "url":format!("https://localhost:{}",f.address.port()),"token":token,
        "ca":ca,"properties":f.temp.path().join("game/server.properties"),"minecraft_version":"1.21.1","pack_format":34
    })).unwrap()).unwrap();
    println!("LAB_RECEIPT={}", path.display());
    // Ends sooner when the tester creates this fixture-owned marker. No server
    // or user directory is kept after the fixture destructor completes.
    for _ in 0..600 {
        if f.temp.path().join("lab-finished").exists() {
            return;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}
