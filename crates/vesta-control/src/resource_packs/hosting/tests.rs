use super::*;
use base64::{Engine, engine::general_purpose::STANDARD};
use std::{
    io::{Cursor, Write},
    time::{Duration, Instant},
};

struct Hooks {
    db: StateDatabase,
    recovery: AtomicBool,
    version: Mutex<String>,
    values: Mutex<BTreeMap<String, String>>,
    stopped: AtomicBool,
    calls: std::sync::atomic::AtomicUsize,
}
impl Hooks {
    fn new(db: StateDatabase) -> Self {
        Self {
            db,
            recovery: AtomicBool::new(false),
            version: Mutex::new("1.21.4".into()),
            values: Mutex::new(BTreeMap::from([("motd".into(), "Keep this".into())])),
            stopped: AtomicBool::new(true),
            calls: Default::default(),
        }
    }
    fn bytes(&self) -> String {
        store::encode(&*self.values.lock().unwrap()).unwrap()
    }
}
impl RuntimeHooks for Hooks {
    fn recovery_block(&self, blocked: bool) -> Result<()> {
        self.recovery.store(blocked, Ordering::Release);
        Ok(())
    }
    fn minecraft_version(&self) -> Result<String> {
        // Match real software-target hooks: they perform their own database read.
        // Calling this inside a service transaction must fail the regression.
        self.db
            .read(|_| Ok(self.version.lock().unwrap().clone()))
            .map_err(Into::into)
    }
    fn properties_hash(&self) -> Result<String> {
        Ok(hash(self.bytes().as_bytes()))
    }
    fn preview(
        &self,
        _: &Subject,
        changes: &BTreeMap<String, String>,
    ) -> Result<(String, String, String)> {
        self.db.read(|_| Ok(()))?;
        let values = self.values.lock().unwrap();
        let mut updated = values.clone();
        updated.extend(changes.clone());
        Ok((
            hash(store::encode(&*values)?.as_bytes()),
            hash(store::encode(&updated)?.as_bytes()),
            values.get("resource-pack").cloned().unwrap_or_default(),
        ))
    }
    fn apply(
        &self,
        _: &Subject,
        _: &str,
        before: &str,
        expected_version: &str,
        changes: &BTreeMap<String, String>,
    ) -> Result<Box<dyn Send>> {
        if self.minecraft_version()? != expected_version {
            return Err(Error::TargetChanged);
        }
        if !self.stopped.load(Ordering::Acquire) {
            return Err(Error::StoppedRequired);
        }
        let mut values = self.values.lock().unwrap();
        if hash(store::encode(&*values)?.as_bytes()) != before {
            return Err(Error::Conflict);
        }
        values.extend(changes.clone());
        self.calls.fetch_add(1, Ordering::Relaxed);
        Ok(Box::new(()))
    }
}
struct Fixture {
    _serial: std::sync::MutexGuard<'static, ()>,
    _temp: tempfile::TempDir,
    db: StateDatabase,
    actor: Subject,
    service: Option<Service>,
    hooks: Arc<Hooks>,
}
impl Fixture {
    fn new() -> Self {
        let serial = super::super::tests::SERIAL
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let temp = tempfile::tempdir().unwrap();
        let db = StateDatabase::open(
            vesta_state::StateDirectory::new(temp.path().join("state"), None).unwrap(),
            Arc::new(vesta_state::SystemClock),
        )
        .unwrap();
        let principal = uuid::Uuid::new_v4().to_string();
        let device = uuid::Uuid::new_v4().to_string();
        db.write(|c|{let now=db.clock().millis();
            c.execute("INSERT INTO principals(id,server_id,display_name,status,created_at,updated_at) VALUES(?,?,'Owner','active',?,?)",params![principal,db.server_id(),now,now])?;
            c.execute("INSERT INTO devices(id,server_id,principal_id,label,public_key_sha256,created_at) VALUES(?,?,?,'Test device',?,?)",params![device,db.server_id(),principal,hash(device.as_bytes()),now])?;
            c.execute("INSERT INTO role_assignments(id,principal_id,role_id,granted_by,created_at) VALUES(?,?,'owner','fixture',?)",params![uuid::Uuid::new_v4().to_string(),principal,now])?;Ok(())}).unwrap();
        let actor = Subject {
            principal: crate::identity::PrincipalId::parse(&principal).unwrap(),
            device: crate::identity::DeviceId::parse(&device).unwrap(),
        };
        let hooks = Arc::new(Hooks::new(db.clone()));
        let service = Service::open(db.clone(), hooks.clone()).unwrap();
        Self {
            _serial: serial,
            _temp: temp,
            db,
            actor,
            service: Some(service),
            hooks,
        }
    }
    fn handle(&self) -> Handle {
        self.service.as_ref().unwrap().handle()
    }
    fn upload(&self, bytes: &[u8], key: &str) -> String {
        let h = self.handle();
        let sha = hash(bytes);
        let u = h
            .upload_begin(
                &self.actor,
                &format!("{key}-begin"),
                &UploadBegin {
                    size: bytes.len().to_string(),
                    sha256: sha.clone(),
                },
            )
            .unwrap();
        h.upload_chunk(
            &self.actor,
            &format!("{key}-chunk"),
            &UploadChunk {
                upload_id: u.id.clone(),
                offset: "0".into(),
                content_base64: STANDARD.encode(bytes),
            },
        )
        .unwrap();
        h.upload_finish(&self.actor, &format!("{key}-finish"), &u.id)
            .unwrap();
        sha
    }
    fn settings(&self, sha: &str) -> Settings {
        Settings {
            public_origin: "https://packs.example.invalid".into(),
            minecraft_version: "1.21.4".into(),
            format: PackFormat::new(46, 0).unwrap(),
            description: "Test pack".into(),
            required: false,
            prompt: String::new(),
            layers: vec![Layer {
                id: "first".into(),
                source: Source::Upload { sha256: sha.into() },
            }],
        }
    }
    fn release(&self) -> Release {
        let sha = self.upload(&zip(), "fixture");
        let h = self.handle();
        h.save(
            &self.actor,
            "save",
            &Save {
                expected_revision: "0".into(),
                settings: self.settings(&sha),
            },
        )
        .unwrap();
        let job = h
            .build(
                &self.actor,
                "build",
                &BuildRequest {
                    expected_revision: "1".into(),
                },
            )
            .unwrap();
        let until = Instant::now() + Duration::from_secs(10);
        loop {
            let job = h
                .status(&self.actor)
                .unwrap()
                .jobs
                .into_iter()
                .find(|j| j.id == job.id)
                .unwrap();
            if job.state == "completed" {
                return job.release.unwrap();
            }
            assert_ne!(job.state, "failed", "{:?}", job.error);
            assert!(Instant::now() < until, "job stalled");
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
fn zip() -> Vec<u8> {
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for (name, bytes) in [
        (
            "pack.mcmeta",
            br#"{"pack":{"pack_format":46,"description":"Fixture"}}"#.as_slice(),
        ),
        (
            "assets/test/lang/en_us.json",
            br#"{"test":"hello"}"#.as_slice(),
        ),
    ] {
        zip.start_file(name, zip::write::SimpleFileOptions::default())
            .unwrap();
        zip.write_all(bytes).unwrap();
    }
    zip.finish().unwrap().into_inner()
}
#[test]
fn draft_upload_build_apply_and_replay_survive_service_reopen() {
    let mut f = Fixture::new();
    let release = f.release();
    let h = f.handle();
    assert!(h.status(&f.actor).unwrap().active.is_none());
    let plan = h
        .plan(
            &f.actor,
            &PlanRequest {
                release_id: release.id.clone(),
                expected_revision: "1".into(),
            },
        )
        .unwrap();
    let request = Apply {
        plan_id: plan.plan_id,
        confirmation_token: plan.confirmation_token,
        acknowledge_external_pack: false,
    };
    let result = h.apply(&f.actor, "apply", &request).unwrap();
    assert_eq!(result.release.id, release.id);
    assert_eq!(f.hooks.values.lock().unwrap()["motd"], "Keep this");
    assert_eq!(
        f.hooks.values.lock().unwrap()["resource-pack-sha1"],
        release.artifact.sha1()
    );
    drop(h);
    drop(f.service.take());
    f.service = Some(Service::open(f.db.clone(), f.hooks.clone()).unwrap());
    assert_eq!(
        f.handle()
            .apply(&f.actor, "apply", &request)
            .unwrap()
            .release
            .id,
        release.id
    );
    assert_eq!(f.hooks.calls.load(Ordering::Relaxed), 1);
    assert_eq!(
        f.handle().status(&f.actor).unwrap().draft.pack_id,
        release.pack_id
    );
    let (mut file, _) = f.handle().open_public(release.artifact.sha256()).unwrap();
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(&mut file, &mut bytes).unwrap();
    assert_eq!(hash(&bytes), release.artifact.sha256());
}
#[test]
fn uploads_reject_wrong_offsets_bad_hashes_and_cross_device_access() {
    let f = Fixture::new();
    let h = f.handle();
    let bytes = zip();
    let u = h
        .upload_begin(
            &f.actor,
            "begin",
            &UploadBegin {
                size: bytes.len().to_string(),
                sha256: hash(&bytes),
            },
        )
        .unwrap();
    let bad = UploadChunk {
        upload_id: u.id.clone(),
        offset: "1".into(),
        content_base64: STANDARD.encode(&bytes),
    };
    assert_eq!(
        h.upload_chunk(&f.actor, "bad", &bad).unwrap_err(),
        Error::Conflict
    );
    assert_eq!(
        h.upload_finish(&f.actor, "early", &u.id).unwrap_err(),
        Error::Conflict
    );
    let other_id = uuid::Uuid::new_v4().to_string();
    f.db.write(|c|{c.execute("INSERT INTO devices(id,server_id,principal_id,label,public_key_sha256,created_at) VALUES(?,?,?,'Second device',?,?)",
        params![other_id,f.db.server_id(),f.actor.principal.as_str(),hash(other_id.as_bytes()),f.db.clock().millis()])?;Ok(())}).unwrap();
    let other = Subject {
        principal: f.actor.principal.clone(),
        device: crate::identity::DeviceId::parse(&other_id).unwrap(),
    };
    assert_eq!(
        h.upload_finish(&other, "foreign-finish", &u.id)
            .unwrap_err(),
        Error::NotFound
    );
    let split = bytes.len() / 2;
    let chunk = UploadChunk {
        upload_id: u.id.clone(),
        offset: "0".into(),
        content_base64: STANDARD.encode(&bytes[..split]),
    };
    let first = h.upload_chunk(&f.actor, "chunk", &chunk).unwrap();
    assert_eq!(
        first.received,
        h.upload_chunk(&f.actor, "chunk", &chunk).unwrap().received
    );
    h.upload_chunk(
        &f.actor,
        "second-chunk",
        &UploadChunk {
            upload_id: u.id.clone(),
            offset: split.to_string(),
            content_base64: STANDARD.encode(&bytes[split..]),
        },
    )
    .unwrap();
    let receipt = h.upload_finish(&f.actor, "finish", &u.id).unwrap();
    assert_eq!(
        receipt.sha256,
        h.upload_finish(&f.actor, "finish", &u.id).unwrap().sha256
    );
    let altered = UploadBegin {
        size: "1".into(),
        sha256: "0".repeat(64),
    };
    assert_eq!(
        h.upload_begin(&f.actor, "begin", &altered).unwrap_err(),
        Error::IdempotencyConflict
    );
    let bad = h.upload_begin(&f.actor, "bad-hash", &altered).unwrap();
    h.upload_chunk(
        &f.actor,
        "bad-content",
        &UploadChunk {
            upload_id: bad.id.clone(),
            offset: "0".into(),
            content_base64: STANDARD.encode(b"x"),
        },
    )
    .unwrap();
    assert_eq!(
        h.upload_finish(&f.actor, "bad-finish", &bad.id)
            .unwrap_err(),
        Error::Conflict
    );
    assert_eq!(h.upload_cancel(&f.actor, "cancel", &bad.id), Ok(true));
}
#[test]
fn stale_drafts_revocation_invalid_origin_and_target_drift_refuse() {
    let f = Fixture::new();
    let h = f.handle();
    let release = f.release();
    let mut s = release.settings.clone();
    s.public_origin = "http://bad.invalid".into();
    assert_eq!(
        h.save(
            &f.actor,
            "bad-origin",
            &Save {
                expected_revision: "1".into(),
                settings: s
            }
        )
        .unwrap_err(),
        Error::Invalid
    );
    assert_eq!(
        h.save(
            &f.actor,
            "stale",
            &Save {
                expected_revision: "0".into(),
                settings: release.settings
            }
        )
        .unwrap_err(),
        Error::Conflict
    );
    *f.hooks.version.lock().unwrap() = "1.21.5".into();
    assert_eq!(
        h.plan(
            &f.actor,
            &PlanRequest {
                release_id: release.id,
                expected_revision: "1".into()
            }
        )
        .unwrap_err(),
        Error::TargetChanged
    );
    f.db.write(|c| {
        c.execute(
            "UPDATE devices SET revoked_at=? WHERE id=?",
            params![f.db.clock().millis(), f.actor.device.as_str()],
        )?;
        Ok(())
    })
    .unwrap();
    assert_eq!(h.status(&f.actor).unwrap_err(), Error::Forbidden);
}
#[test]
fn external_pack_requires_explicit_takeover_and_running_game_never_applies() {
    let f = Fixture::new();
    let release = f.release();
    let h = f.handle();
    f.hooks.values.lock().unwrap().insert(
        "resource-pack".into(),
        "https://external.invalid/pack.zip".into(),
    );
    let plan = h
        .plan(
            &f.actor,
            &PlanRequest {
                release_id: release.id,
                expected_revision: "1".into(),
            },
        )
        .unwrap();
    assert!(plan.replaces_external_pack);
    let mut request = Apply {
        plan_id: plan.plan_id,
        confirmation_token: plan.confirmation_token,
        acknowledge_external_pack: false,
    };
    assert_eq!(
        h.apply(&f.actor, "apply", &request).unwrap_err(),
        Error::Confirmation
    );
    request.acknowledge_external_pack = true;
    f.hooks.stopped.store(false, Ordering::Release);
    assert_eq!(
        h.apply(&f.actor, "apply", &request).unwrap_err(),
        Error::StoppedRequired
    );
    assert!(h.status(&f.actor).unwrap().active.is_none());
    f.hooks.stopped.store(true, Ordering::Release);
    h.apply(&f.actor, "apply", &request).unwrap();
}
#[test]
fn interrupted_apply_reconciles_and_commits_receipt_with_active_release() {
    let mut f = Fixture::new();
    let release = f.release();
    let h = f.handle();
    let changes = changes(&release).unwrap();
    let (before, after, _) = f.hooks.preview(&f.actor, &changes).unwrap();
    let request = Apply {
        plan_id: "lost-plan".into(),
        confirmation_token: "lost-token".into(),
        acknowledge_external_pack: false,
    };
    let pending = Pending {
        release: release.clone(),
        before: before.clone(),
        after,
        principal: f.actor.principal.as_str().into(),
        device: f.actor.device.as_str().into(),
        key: "recover".into(),
        digest: hash(store::encode(&("apply", &request)).unwrap().as_bytes()),
    };
    f.db.write(|c| {
        c.execute(
            "UPDATE hosted_pack_state SET pending=?",
            [store::encode(&pending).unwrap()],
        )?;
        Ok(())
    })
    .unwrap();
    f.hooks
        .apply(&f.actor, "fixture", &before, "1.21.4", &changes)
        .unwrap();
    drop(h);
    drop(f.service.take());
    f.service = Some(Service::open(f.db.clone(), f.hooks.clone()).unwrap());
    assert_eq!(
        f.handle()
            .apply(&f.actor, "recover", &request)
            .unwrap()
            .release
            .id,
        release.id
    );
    assert_eq!(f.hooks.calls.load(Ordering::Relaxed), 1);
}
#[test]
fn version_specific_keys_and_origins_are_strict() {
    assert_eq!(version_features("1.16.5").unwrap(), (false, false));
    assert_eq!(version_features("1.17").unwrap(), (true, false));
    assert_eq!(version_features("1.20.2").unwrap(), (true, false));
    assert_eq!(version_features("1.20.3").unwrap(), (true, true));
    assert_eq!(version_features("26.3").unwrap(), (true, true));
    assert!(version_features("25w01a").is_err());
    for url in [
        "http://a.invalid",
        "https://a.invalid/path",
        "https://name:pass@a.invalid",
        "https://a.invalid/?token=x",
        "https://a.invalid/#x",
    ] {
        assert!(origin(url).is_err(), "{url}");
    }
}

#[test]
fn apply_plans_bind_device_expiry_and_draft_revision_before_any_effect() {
    let f = Fixture::new();
    let release = f.release();
    let h = f.handle();
    let plan = h
        .plan(
            &f.actor,
            &PlanRequest {
                release_id: release.id.clone(),
                expected_revision: "1".into(),
            },
        )
        .unwrap();
    let request = Apply {
        plan_id: plan.plan_id.clone(),
        confirmation_token: plan.confirmation_token,
        acknowledge_external_pack: false,
    };
    let device = uuid::Uuid::new_v4().to_string();
    f.db.write(|c|{c.execute("INSERT INTO devices(id,server_id,principal_id,label,public_key_sha256,created_at) VALUES(?,?,?,'Other device',?,?)",params![device,f.db.server_id(),f.actor.principal.as_str(),hash(device.as_bytes()),f.db.clock().millis()])?;Ok(())}).unwrap();
    let other = Subject {
        principal: f.actor.principal.clone(),
        device: crate::identity::DeviceId::parse(&device).unwrap(),
    };
    assert_eq!(
        h.apply(&other, "other-apply", &request).unwrap_err(),
        Error::Confirmation
    );
    h.inner
        .plans
        .lock()
        .unwrap()
        .get_mut(&plan.plan_id)
        .unwrap()
        .expires = 0;
    assert_eq!(
        h.apply(&f.actor, "expired-apply", &request).unwrap_err(),
        Error::Confirmation
    );
    let plan = h
        .plan(
            &f.actor,
            &PlanRequest {
                release_id: release.id,
                expected_revision: "1".into(),
            },
        )
        .unwrap();
    let request = Apply {
        plan_id: plan.plan_id,
        confirmation_token: plan.confirmation_token,
        acknowledge_external_pack: false,
    };
    h.save(
        &f.actor,
        "new-draft",
        &Save {
            expected_revision: "1".into(),
            settings: release.settings,
        },
    )
    .unwrap();
    assert_eq!(
        h.apply(&f.actor, "stale-apply", &request).unwrap_err(),
        Error::Conflict
    );
    assert_eq!(f.hooks.calls.load(Ordering::Relaxed), 0);
    assert!(h.status(&f.actor).unwrap().active.is_none());
}

#[test]
fn ambiguous_activation_leaves_management_available_and_can_be_explicitly_abandoned() {
    let mut f = Fixture::new();
    let release = f.release();
    let h = f.handle();
    let plan = h
        .plan(
            &f.actor,
            &PlanRequest {
                release_id: release.id.clone(),
                expected_revision: "1".into(),
            },
        )
        .unwrap();
    h.apply(
        &f.actor,
        "initial-apply",
        &Apply {
            plan_id: plan.plan_id,
            confirmation_token: plan.confirmation_token,
            acknowledge_external_pack: false,
        },
    )
    .unwrap();
    let pending = Pending {
        release: release.clone(),
        before: "0".repeat(64),
        after: "1".repeat(64),
        principal: f.actor.principal.as_str().into(),
        device: f.actor.device.as_str().into(),
        key: "interrupted".into(),
        digest: "2".repeat(64),
    };
    let insert = || {
        f.db.write(|c| {
            c.execute(
                "UPDATE hosted_pack_state SET pending=?,recovery=1",
                [store::encode(&pending).unwrap()],
            )?;
            Ok(())
        })
        .unwrap();
    };
    insert();
    drop(h);
    drop(f.service.take());
    f.service = Some(Service::open(f.db.clone(), f.hooks.clone()).unwrap());
    let h = f.handle();
    assert!(h.status(&f.actor).unwrap().recovery_required);
    assert!(f.hooks.recovery.load(Ordering::Acquire));
    let original = f.hooks.bytes();
    let mut input = AbandonRecovery {
        expected_properties_sha256: "0".repeat(64),
        confirm_abandon: true,
    };
    assert_eq!(
        h.abandon_recovery(&f.actor, "wrong-current", &input)
            .unwrap_err(),
        Error::Conflict
    );
    input.expected_properties_sha256 = hash(original.as_bytes());
    assert_eq!(h.abandon_recovery(&f.actor, "abandon", &input), Ok(true));
    assert_eq!(f.hooks.bytes(), original);
    assert!(!f.hooks.recovery.load(Ordering::Acquire));
    assert!(h.status(&f.actor).unwrap().active.is_none());
    assert!(h.open_public(release.artifact.sha256()).is_ok());
    // Replaying an earlier successful abandon must not unblock a new incident.
    f.db.write(|c| {
        c.execute(
            "UPDATE hosted_pack_state SET pending=?,recovery=1",
            [store::encode(&pending).unwrap()],
        )?;
        Ok(())
    })
    .unwrap();
    f.hooks.recovery_block(true).unwrap();
    assert_eq!(h.abandon_recovery(&f.actor, "abandon", &input), Ok(true));
    assert!(f.hooks.recovery.load(Ordering::Acquire));
}
