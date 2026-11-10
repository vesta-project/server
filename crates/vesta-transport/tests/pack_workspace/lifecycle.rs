//! Owned loopback lifecycle fixtures; default cases have no provider/runtime IO.
//! The explicitly ignored retained-resource case performs bounded public downloads.
use super::*;
use base64::{Engine, engine::general_purpose::STANDARD};

struct Session {
    f: Fixture,
    token: String,
    device: String,
    key: ring::signature::Ed25519KeyPair,
    revision: String,
    intent: usize,
}
impl Session {
    fn new() -> Self {
        let f = audit_software_fixture();
        // Model an already selected runtime. It is inert, never executed, and is
        // not evidence of managed runtime installation or Minecraft compatibility.
        f.host().database().write(|c|{
            c.execute("UPDATE software_state SET current_launch=json_set(current_launch,'$.installation.software','vanilla','$.installation.version','1.21.1') WHERE singleton=1",[])?;
            Ok(())
        }).unwrap();
        std::fs::write(
            f.temp.path().join("game/server.properties"),
            b"motd=owned-lifecycle-witness\n",
        )
        .unwrap();
        let (device, key) = enroll_management(&f, vesta_control::identity::BuiltinRole::Owner);
        let token = management_session(&f, &device, &key);
        Self {
            f,
            token,
            device,
            key,
            revision: "0".into(),
            intent: 0,
        }
    }
    fn post(&mut self, route: &str, input: &serde_json::Value) -> WireReply {
        self.intent += 1;
        audit_long_post(
            &self.f,
            &self.token,
            route,
            Some(&format!("lifecycle-{:08}", self.intent)),
            input,
        )
    }
    fn data(reply: WireReply) -> serde_json::Value {
        assert_eq!(
            reply.status,
            200,
            "{}",
            String::from_utf8_lossy(&reply.body)
        );
        reply.json()["data"].clone()
    }
    fn get(&self, route: &str) -> serde_json::Value {
        Self::data(audit_long_get(&self.f, &self.token, route))
    }
    fn write(&mut self, root: &str, path: &str, bytes: &[u8]) {
        let input = json!({"expected_revision":self.revision,"change":{"operation":"write","root":root,"path":path,"content_base64":STANDARD.encode(bytes)}});
        let value = Self::data(self.post("pack/files/mutate", &input));
        self.revision = value["revision"].as_str().unwrap().into();
    }
    fn attach(&mut self, root: &str, path: &str, bytes: &[u8]) {
        let hash = Self::data(self.post(
            "pack/draft/upload",
            &json!({"content_base64":STANDARD.encode(bytes)}),
        ));
        assert!(hash.is_string());
        let input = json!({"expected_revision":self.revision,"change":{"operation":"attach","root":root,"path":path,"sha256":hash}});
        let value = Self::data(self.post("pack/files/mutate", &input));
        self.revision = value["revision"].as_str().unwrap().into();
    }
    fn trash(&mut self, path: &str) {
        let input = json!({"expected_revision":self.revision,"change":{"operation":"trash","root":"overrides","path":path}});
        let value = Self::data(self.post("pack/files/mutate", &input));
        self.revision = value["revision"].as_str().unwrap().into();
    }
    fn version(&mut self, version: &str) {
        let mut manifest = self.get("pack/draft")["draft"]["manifest"].clone();
        manifest["display_version"] = json!(version);
        let input = json!({"expected_revision":self.revision,"manifest":manifest});
        let value = Self::data(self.post("pack/draft", &input));
        self.revision = value["revision"].as_str().unwrap().into();
    }
    fn plan(&self) -> serde_json::Value {
        let input = json!({"analyze":true,"source":{"source":"draft","revision":self.revision}});
        let start = Instant::now();
        let value = Self::data(audit_long_post(
            &self.f,
            &self.token,
            "pack/install/plan",
            None,
            &input,
        ));
        assert_eq!(value["valid"], true);
        assert_eq!(value["software"], "vanilla");
        assert_eq!(value["minecraft_version"], "1.21.1");
        assert!(value["loader_version"].is_null());
        assert_eq!(value["software_switch_required"], false);
        eprintln!(
            "LIFECYCLE plan files={} ms={:.3}",
            value["files"].as_array().unwrap().len(),
            start.elapsed().as_secs_f64() * 1000.
        );
        value
    }
    fn apply_body(plan: &serde_json::Value, choice: &str) -> serde_json::Value {
        let choices: Vec<_> = plan["files"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|f| !f["conflict"].is_null())
            .map(|f| json!({"path":f["path"],"choice":choice}))
            .collect();
        json!({"plan_id":plan["plan_id"],"confirmation_token":plan["confirmation_token"],"acknowledge_warnings":true,"acknowledge_software_switch":false,"choices":choices,"set_as_modpack":false})
    }
    fn terminal(&self, id: &str) -> serde_json::Value {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let jobs = self.get("software/jobs");
            let job = jobs["jobs"]
                .as_array()
                .unwrap()
                .iter()
                .find(|j| j["id"] == id)
                .unwrap()
                .clone();
            if matches!(
                job["state"].as_str(),
                Some("succeeded" | "failed" | "recovery_required")
            ) {
                return job;
            }
            assert!(
                Instant::now() < deadline,
                "bounded fixture job did not settle"
            );
            std::thread::sleep(Duration::from_millis(25));
        }
    }
    fn apply(&mut self, plan: &serde_json::Value, choice: &str) -> String {
        let start = Instant::now();
        let value = Self::data(self.post("pack/install/apply", &Self::apply_body(plan, choice)));
        let id = value["software_job_id"].as_str().unwrap().to_owned();
        let job = self.terminal(&id);
        assert_eq!(job["state"], "succeeded", "{job}");
        eprintln!(
            "LIFECYCLE apply files={} ms={:.3}",
            plan["files"].as_array().unwrap().len(),
            start.elapsed().as_secs_f64() * 1000.
        );
        assert_eq!(self.get("pack/server-link")["state"], "installed");
        assert_eq!(
            std::fs::read(self.f.temp.path().join("game/server.properties")).unwrap(),
            b"motd=owned-lifecycle-witness\n"
        );
        id
    }
    fn bytes(&self, path: &str) -> Vec<u8> {
        std::fs::read(self.f.temp.path().join("game").join(path)).unwrap()
    }
    fn assert_bytes(&self, path: &str, expected: &[u8]) {
        assert_eq!(self.bytes(path), expected, "{path}");
    }
    fn reopen(&mut self) {
        audit_reopen_host(&mut self.f);
        assert_eq!(
            audit_long_get(&self.f, &self.token, "software/jobs").status,
            401
        );
        self.token = management_session(&self.f, &self.device, &self.key);
    }
}

#[test]
fn versions_retry_upgrade_downgrade_and_untracked_witnesses() {
    let mut s = Session::new();
    for (path, bytes) in [
        ("mods/custom.jar", b"custom mod".as_slice()),
        ("config/custom.json", b"{\"custom\":true}"),
        ("world/region/witness.mca", b"world witness"),
        ("world/datapacks/custom.zip.disabled", b"custom datapack"),
    ] {
        let p = s.f.temp.path().join("game").join(path);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, bytes).unwrap();
    }
    s.version("1.0");
    s.write("overrides", "mods/managed.jar.disabled", b"binary-v1");
    s.write(
        "overrides",
        "config/settings.json",
        br#"{"pack":1,"user":0}"#,
    );
    s.write("overrides", "config/removed.json", b"{}");
    let first = s.plan();
    s.apply(&first, "replace");
    let repeated = s.plan();
    assert!(
        repeated["files"]
            .as_array()
            .unwrap()
            .iter()
            .all(|f| f["operation"] == "unchanged")
    );
    s.apply(&repeated, "replace");
    s.assert_bytes("mods/managed.jar.disabled", b"binary-v1");
    std::fs::write(
        s.f.temp.path().join("game/config/settings.json"),
        br#"{"pack":1,"user":99}"#,
    )
    .unwrap();
    s.version("2.0");
    s.trash("mods/managed.jar.disabled");
    s.trash("config/removed.json");
    s.write("overrides", "mods/renamed.jar.disabled", b"binary-v2");
    s.write(
        "overrides",
        "config/settings.json",
        br#"{"pack":2,"user":0}"#,
    );
    let next = s.plan();
    assert!(
        next["removed"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r[0] == "mods/managed.jar.disabled")
    );
    assert!(
        next["skipped"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r == "config/removed.json")
    );
    let job = s.apply(&next, "replace");
    assert!(
        !s.f.temp
            .path()
            .join("game/mods/managed.jar.disabled")
            .exists()
    );
    s.assert_bytes("mods/renamed.jar.disabled", b"binary-v2");
    let merged: serde_json::Value =
        serde_json::from_slice(&s.bytes("config/settings.json")).unwrap();
    assert_eq!(merged, json!({"pack":2,"user":99}));
    assert_eq!(s.get("pack/server-link")["display_version"], "2.0");
    s.reopen();
    assert_eq!(s.terminal(&job)["state"], "succeeded");
    // Explicit lower display version is an install selection, not publication.
    s.version("1.0");
    s.trash("mods/renamed.jar.disabled");
    s.write("overrides", "mods/managed.jar.disabled", b"binary-v1");
    s.write(
        "overrides",
        "config/settings.json",
        br#"{"pack":1,"user":0}"#,
    );
    let down = s.plan();
    s.apply(&down, "replace");
    assert_eq!(s.get("pack/server-link")["display_version"], "1.0");
    s.assert_bytes("mods/managed.jar.disabled", b"binary-v1");
    assert!(
        !s.f.temp
            .path()
            .join("game/mods/renamed.jar.disabled")
            .exists()
    );
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&s.bytes("config/settings.json")).unwrap(),
        json!({"pack":1,"user":99})
    );
    s.assert_bytes("config/removed.json", b"{}");
    for (path, bytes) in [
        ("mods/custom.jar", b"custom mod".as_slice()),
        ("config/custom.json", b"{\"custom\":true}"),
        ("world/region/witness.mca", b"world witness"),
        ("world/datapacks/custom.zip.disabled", b"custom datapack"),
    ] {
        s.assert_bytes(path, bytes);
    }
}

#[test]
fn modified_owned_binary_requires_choice_and_keeps_local_bytes() {
    let mut s = Session::new();
    s.write("overrides", "mods/managed.jar.disabled", b"base");
    let p = s.plan();
    s.apply(&p, "replace");
    std::fs::write(
        s.f.temp.path().join("game/mods/managed.jar.disabled"),
        b"user",
    )
    .unwrap();
    s.write("overrides", "mods/managed.jar.disabled", b"author2");
    let p = s.plan();
    assert!(p["files"][0]["conflict"].is_string());
    let mut invalid = Session::apply_body(&p, "keep");
    invalid["choices"] = json!([]);
    let jobs = s.get("software/jobs")["jobs"].as_array().unwrap().len();
    let refused = s.post("pack/install/apply", &invalid);
    assert!(refused.status >= 400);
    assert_eq!(
        s.get("software/jobs")["jobs"].as_array().unwrap().len(),
        jobs
    );
    s.apply(&p, "keep");
    s.assert_bytes("mods/managed.jar.disabled", b"user");
    // Explicit replace on a fresh review is allowed for a custom/draft source.
    let p = s.plan();
    s.apply(&p, "replace");
    s.assert_bytes("mods/managed.jar.disabled", b"author2");
}

#[test]
fn changed_destination_after_review_fails_without_identity_or_file_success() {
    let mut s = Session::new();
    s.write("overrides", "mods/a.jar.disabled", b"pack");
    let p = s.plan();
    std::fs::create_dir_all(s.f.temp.path().join("game/mods")).unwrap();
    std::fs::write(
        s.f.temp.path().join("game/mods/a.jar.disabled"),
        b"arrived after plan",
    )
    .unwrap();
    let body = Session::apply_body(&p, "replace");
    let accepted = Session::data(s.post("pack/install/apply", &body));
    let job = s.terminal(accepted["software_job_id"].as_str().unwrap());
    assert_eq!(job["state"], "failed", "{job}");
    s.assert_bytes("mods/a.jar.disabled", b"arrived after plan");
    let linked = audit_long_get(&s.f, &s.token, "pack/server-link");
    // The existing journal contract retains an unresolved staged intent/link;
    // it must never claim that the incoming identity is installed.
    assert_eq!(linked.status, 200);
    let linked = linked.json()["data"].clone();
    assert_eq!(linked["state"], "staged_until_restart");
    assert!(linked["installed"].is_null());
    assert!(!linked["pending_ids"].as_array().unwrap().is_empty());
    assert!(
        s.f.temp
            .path()
            .join("private-state/content-pending")
            .is_file()
    );
    let content = s.get("content");
    let row = content["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["path"] == "mods/a.jar.disabled")
        .unwrap();
    assert_eq!(row["pending_error"], "file_conflict");
    s.reopen();
    assert_eq!(
        s.terminal(accepted["software_job_id"].as_str().unwrap())["state"],
        "failed"
    );
    assert_eq!(s.get("pack/server-link")["state"], "staged_until_restart");
    assert!(
        s.f.temp
            .path()
            .join("private-state/content-pending")
            .is_file()
    );
    s.assert_bytes("mods/a.jar.disabled", b"arrived after plan");
}

#[test]
fn server_override_selection_and_played_world_rotation_are_persisted() {
    let level = |seed: i64| {
        let mut nbt = vec![10, 0, 0, 10, 0, 4, b'D', b'a', b't', b'a', 4, 0, 10];
        nbt.extend(b"RandomSeed");
        nbt.extend(seed.to_be_bytes());
        nbt.extend([0, 0]);
        let mut gzip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        gzip.write_all(&nbt).unwrap();
        gzip.finish().unwrap()
    };
    let base = level(1);
    let played = level(2);
    let incoming = level(3);
    let mut s = Session::new();
    s.write("overrides", "config/which.json", br#"{"side":"shared"}"#);
    s.write(
        "client-overrides",
        "config/which.json",
        br#"{"side":"client"}"#,
    );
    s.write(
        "server-overrides",
        "config/which.json",
        br#"{"side":"server"}"#,
    );
    s.write("client-overrides", "config/client-only.bin", b"client-only");
    s.attach("overrides", "world/level.dat", &base);
    let p = s.plan();
    assert!(
        !p["files"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f["path"] == "config/client-only.bin")
    );
    s.apply(&p, "replace");
    s.assert_bytes("config/which.json", br#"{"side":"server"}"#);
    assert!(!s.f.temp.path().join("game/config/client-only.bin").exists());
    std::fs::write(s.f.temp.path().join("game/world/level.dat"), &played).unwrap();
    std::fs::create_dir_all(s.f.temp.path().join("game/world/region")).unwrap();
    std::fs::write(
        s.f.temp.path().join("game/world/region/player.mca"),
        b"retained player region",
    )
    .unwrap();
    s.attach("overrides", "world/level.dat", &incoming);
    let p = s.plan();
    let id = s.apply(&p, "replace");
    s.assert_bytes("world/level.dat", &incoming);
    let saves: Vec<_> = std::fs::read_dir(s.f.temp.path().join("game"))
        .unwrap()
        .map(|r| r.unwrap().path())
        .filter(|p| {
            p.file_name()
                .unwrap()
                .to_str()
                .unwrap()
                .starts_with("world_")
        })
        .collect();
    assert_eq!(saves.len(), 1);
    assert_eq!(std::fs::read(saves[0].join("level.dat")).unwrap(), played);
    assert_eq!(
        std::fs::read(saves[0].join("region/player.mca")).unwrap(),
        b"retained player region"
    );
    s.reopen();
    assert_eq!(s.terminal(&id)["state"], "succeeded");
    assert_eq!(
        std::fs::read(saves[0].join("region/player.mca")).unwrap(),
        b"retained player region"
    );
}

#[test]
fn competing_actor_plan_cannot_overwrite_newer_installed_link() {
    let mut s = Session::new();
    s.version("1.0");
    s.write("overrides", "config/version.bin", b"v1");
    let old = s.plan();
    s.version("2.0");
    s.write("overrides", "config/version.bin", b"v2");
    let (device, key) = enroll_management(&s.f, vesta_control::identity::BuiltinRole::Owner);
    let other = management_session(&s.f, &device, &key);
    let request = json!({"analyze":true,"source":{"source":"draft","revision":s.revision}});
    let new = Session::data(audit_long_post(
        &s.f,
        &other,
        "pack/install/plan",
        None,
        &request,
    ));
    let accepted = Session::data(audit_long_post(
        &s.f,
        &other,
        "pack/install/apply",
        Some("other-actor-apply"),
        &Session::apply_body(&new, "replace"),
    ));
    assert_eq!(
        s.terminal(accepted["software_job_id"].as_str().unwrap())["state"],
        "succeeded"
    );
    s.assert_bytes("config/version.bin", b"v2");
    let stale = s.post("pack/install/apply", &Session::apply_body(&old, "replace"));
    if stale.status == 200 {
        let value = stale.json();
        let failed = s.terminal(value["data"]["software_job_id"].as_str().unwrap());
        assert_eq!(failed["state"], "failed", "{failed}");
    } else {
        assert!(
            matches!(stale.status, 409 | 412),
            "{}",
            String::from_utf8_lossy(&stale.body)
        );
    }
    s.assert_bytes("config/version.bin", b"v2");
    assert_eq!(s.get("pack/server-link")["display_version"], "2.0");
}

fn apply_at(address: SocketAddr, token: &str, key: &str, input: &serde_json::Value) -> WireReply {
    let bytes = serde_json::to_vec(input).unwrap();
    let mut stream = connect(address, true).unwrap();
    stream
        .sock
        .set_read_timeout(Some(Duration::from_secs(30)))
        .unwrap();
    write!(stream,"POST /api/v3/manage/pack/install/apply HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {token}\r\nIdempotency-Key: {key}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",bytes.len()).unwrap();
    stream.write_all(&bytes).unwrap();
    stream.flush().unwrap();
    read_reply(stream).unwrap()
}

#[test]
fn simultaneous_same_intent_retries_and_changed_body_have_one_job() {
    let mut s = Session::new();
    s.write("overrides", "config/retry.bin", b"exact bytes");
    let p = s.plan();
    let body = Session::apply_body(&p, "replace");
    let address = s.f.address;
    let token = &s.token;
    let (a, b) = std::thread::scope(|scope| {
        let a = scope.spawn(|| apply_at(address, token, "simultaneous-exact-intent", &body));
        let b = scope.spawn(|| apply_at(address, token, "simultaneous-exact-intent", &body));
        (a.join().unwrap(), b.join().unwrap())
    });
    let a = Session::data(a);
    let b = Session::data(b);
    assert_eq!(a["software_job_id"], b["software_job_id"]);
    assert_eq!(
        s.terminal(a["software_job_id"].as_str().unwrap())["state"],
        "succeeded"
    );
    assert_eq!(s.get("software/jobs")["jobs"].as_array().unwrap().len(), 1);
    s.assert_bytes("config/retry.bin", b"exact bytes");
    let mut changed = body;
    changed["set_as_modpack"] = json!(true);
    let refused = audit_long_post(
        &s.f,
        &s.token,
        "pack/install/apply",
        Some("simultaneous-exact-intent"),
        &changed,
    );
    assert_eq!(
        refused.status,
        409,
        "{}",
        String::from_utf8_lossy(&refused.body)
    );
    assert_eq!(s.get("software/jobs")["jobs"].as_array().unwrap().len(), 1);
    s.reopen();
    assert_eq!(
        s.terminal(a["software_job_id"].as_str().unwrap())["state"],
        "succeeded"
    );
}

#[test]
fn distinct_intents_competing_for_one_review_cannot_both_apply() {
    let mut s = Session::new();
    s.write(
        "overrides",
        "config/exclusive.bin",
        b"single reviewed effect",
    );
    let p = s.plan();
    let body = Session::apply_body(&p, "replace");
    let address = s.f.address;
    let token = &s.token;
    let (a, b) = std::thread::scope(|scope| {
        let a = scope.spawn(|| apply_at(address, token, "different-intent-one", &body));
        let b = scope.spawn(|| apply_at(address, token, "different-intent-two", &body));
        (a.join().unwrap(), b.join().unwrap())
    });
    assert_eq!(
        usize::from(a.status == 200) + usize::from(b.status == 200),
        1
    );
    let (accepted, rejected) = if a.status == 200 { (a, b) } else { (b, a) };
    assert_eq!(
        rejected.status,
        409,
        "{}",
        String::from_utf8_lossy(&rejected.body)
    );
    let job = Session::data(accepted)["software_job_id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(s.terminal(&job)["state"], "succeeded");
    assert_eq!(s.get("software/jobs")["jobs"].as_array().unwrap().len(), 1);
    s.assert_bytes("config/exclusive.bin", b"single reviewed effect");
}

#[test]
#[ignore = "one bounded real retained Modrinth resource; actual public provider/artifact request, no Java/game"]
fn renamed_disabled_retained_resource_installs_through_endpoints() {
    use sha2::{Digest, Sha512};
    let mut s = Session::new();
    s.f.host().database().write(|c|{
        c.execute("UPDATE software_state SET current_launch=json_set(current_launch,'$.installation.software','fabric','$.installation.loader_version','0.18.4') WHERE singleton=1",[])?;Ok(())
    }).unwrap();
    // Exact resource from retained COBBLEVERSE1.7.42 index/version metadata.
    // Filtered/renamed one-resource fixture, NOT a complete real pack install.
    let digest = "7fe69d9a90a4f1f19e3a121b0c499f26ba0ca9245ca409b4f8888c42ea810c49afeb9375e73fa840a0f23ac14e22a43233edffb65384389388f6cbbef8eeaf94";
    let index = json!({"formatVersion":1,"game":"minecraft","versionId":"retained-resource-fixture","name":"Retained resource lifecycle",
        "dependencies":{"minecraft":"1.21.1","fabric-loader":"0.18.4"},
        "files":[{"path":"mods/cobblecuisine-renamed.jar.disabled",
            "hashes":{"sha1":"31cf544fe61bb9c21fa8dafe5c03a428049a0f37","sha512":digest},
            "downloads":["https://cdn.modrinth.com/data/dlx62Wp3/versions/KQwSroDP/cobblecuisine-2.0.1-1.7-rc1.jar"],
            "fileSize":683115,"env":{"client":"unsupported","server":"required"}}]});
    let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    zip.start_file(
        "modrinth.index.json",
        zip::write::SimpleFileOptions::default(),
    )
    .unwrap();
    zip.write_all(&serde_json::to_vec(&index).unwrap()).unwrap();
    for (path, bytes) in [
        ("overrides/config/side.bin", b"shared".as_slice()),
        ("server-overrides/config/side.bin", b"server"),
        ("client-overrides/config/side.bin", b"client"),
    ] {
        zip.start_file(path, zip::write::SimpleFileOptions::default())
            .unwrap();
        zip.write_all(bytes).unwrap();
    }
    let bytes = zip.finish().unwrap().into_inner();
    let hash = hex::encode(sha2::Sha256::digest(&bytes));
    Session::data(s.post("pack/draft/uploads/begin",&json!({"upload_id":"retained-small","total_size":bytes.len().to_string(),"sha256":hash,"archive":true})));
    Session::data(s.post("pack/draft/uploads/chunk",&json!({"upload_id":"retained-small","offset":"0","content_base64":STANDARD.encode(&bytes)})));
    Session::data(s.post(
        "pack/draft/uploads/finish",
        &json!({"upload_id":"retained-small"}),
    ));
    let plan = Session::data(audit_long_post(
        &s.f,
        &s.token,
        "pack/install/plan",
        None,
        &json!({"analyze":true,"source":{"source":"uploaded_archive","upload_id":"retained-small"}}),
    ));
    assert_eq!(plan["valid"], true);
    assert_eq!(plan["software"], "fabric");
    assert_eq!(plan["loader_version"], "0.18.4");
    assert_eq!(plan["software_switch_required"], false);
    assert!(
        plan["files"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f["path"] == "mods/cobblecuisine-renamed.jar.disabled")
    );
    let start = Instant::now();
    let id = s.apply(&plan, "replace");
    let installed = s.bytes("mods/cobblecuisine-renamed.jar.disabled");
    assert_eq!(installed.len(), 683115);
    assert_eq!(hex::encode(Sha512::digest(&installed)), digest);
    s.assert_bytes("config/side.bin", b"server");
    let inventory = s.get("content");
    let item = inventory["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["path"] == "mods/cobblecuisine-renamed.jar.disabled")
        .unwrap();
    assert_eq!(item["enabled"], false);
    assert_eq!(item["provider"]["project_id"], "dlx62Wp3");
    assert_eq!(item["provider"]["version_id"], "KQwSroDP");
    s.reopen();
    assert_eq!(s.terminal(&id)["state"], "succeeded");
    assert_eq!(
        hex::encode(Sha512::digest(
            s.bytes("mods/cobblecuisine-renamed.jar.disabled")
        )),
        digest
    );
    eprintln!(
        "LIFECYCLE_REAL retained_resource=dlx62Wp3/KQwSroDP bytes=683115 sha512_verified=true elapsed_ms={:.3}",
        start.elapsed().as_secs_f64() * 1000.
    );
}
