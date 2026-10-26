use super::*;
use std::sync::Arc;
use vesta_state::{Clock, StateDirectory};
struct TestClock;
impl Clock for TestClock {
    fn millis(&self) -> i64 {
        1_791_115_200_000
    }
}
pub(super) struct Fixture {
    pub temp: tempfile::TempDir,
    pub db: StateDatabase,
    pub subject: Subject,
    pub workspace: PackWorkspace,
}
impl Fixture {
    pub fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let db = StateDatabase::open(
            StateDirectory::new(temp.path().join("state"), None).unwrap(),
            Arc::new(TestClock),
        )
        .unwrap();
        let principal = uuid::Uuid::new_v4().to_string();
        let device = uuid::Uuid::new_v4().to_string();
        db.write(|c| {
            c.execute("INSERT INTO principals(id,server_id,display_name,status,created_at,updated_at) VALUES(?,?,'Pack owner','active',?,?)",params![principal,db.server_id(),db.clock().millis(),db.clock().millis()])?;
            c.execute("INSERT INTO devices(id,server_id,principal_id,label,public_key_sha256,created_at) VALUES(?,?,?,'Pack device',?,?)",params![device,db.server_id(),principal,digest(device.as_bytes()),db.clock().millis()])?;
            c.execute("INSERT INTO role_assignments(id,principal_id,role_id,granted_by,created_at) VALUES(?,?,'owner','fixture',?)",params![uuid::Uuid::new_v4().to_string(),principal,db.clock().millis()])?;Ok(())
        }).unwrap();
        let subject = Subject {
            principal: crate::identity::PrincipalId::parse(&principal).unwrap(),
            device: crate::identity::DeviceId::parse(&device).unwrap(),
        };
        let workspace = PackWorkspace::open(&db).unwrap();
        Self {
            temp,
            db,
            subject,
            workspace,
        }
    }
    pub fn manifest(&self) -> PackManifest {
        self.workspace
            .read(&self.subject, "1.21.1")
            .unwrap()
            .manifest
    }
}
#[test]
fn draft_is_durable_revisioned_idempotent_and_separate_from_server_files() {
    let f = Fixture::new();
    let mut manifest = f.manifest();
    manifest.loader = ClientLoader::Fabric;
    manifest.loader_version = Some("0.16.10".into());
    let input = DraftWrite {
        expected_revision: "0".into(),
        manifest,
    };
    let first = f
        .workspace
        .write(&f.subject, "1.21.1", "write-1", &input)
        .unwrap();
    assert_eq!(first.revision, "1");
    assert_eq!(
        f.workspace
            .write(&f.subject, "1.21.1", "write-1", &input)
            .unwrap(),
        first
    );
    assert_eq!(
        f.workspace
            .write(&f.subject, "1.21.1", "write-2", &input)
            .unwrap_err()
            .code(),
        "pack_release_conflict"
    );
    let mut changed = input.clone();
    changed.manifest.name = "Other".into();
    assert_eq!(
        f.workspace
            .write(&f.subject, "1.21.1", "write-1", &changed)
            .unwrap_err()
            .code(),
        "idempotency_conflict"
    );
    assert_eq!(
        PackWorkspace::open(&f.db)
            .unwrap()
            .read(&f.subject, "1.21.1")
            .unwrap(),
        first
    );
    assert!(
        super::super::PackReleases::open(&f.db)
            .unwrap()
            .current()
            .unwrap()
            .is_none()
    );
    assert!(!f.temp.path().join("game").exists());
    assert_eq!(
        f.workspace
            .delete(&f.subject, "1.21.1", "delete", "1")
            .unwrap()
            .revision,
        "2"
    );
}
#[test]
fn uploads_are_hashed_and_reference_checked_and_drift_is_visible() {
    let f = Fixture::new();
    let upload = UploadRequest {
        content_base64: STANDARD.encode(b"client config"),
    };
    let sha = f.workspace.upload(&f.subject, "blob", &upload).unwrap();
    assert_eq!(sha, digest(b"client config"));
    let mut m = f.manifest();
    m.entries.push(PackEntry {
        id: "config".into(),
        kind: EntryKind::ConfigOverride,
        path: "config/example.toml".into(),
        required: true,
        environment: Environment {
            client: Side::Required,
            server: Side::Unsupported,
        },
        source: EntrySource::Upload { sha256: sha },
    });
    f.workspace
        .write(
            &f.subject,
            "1.21.1",
            "save",
            &DraftWrite {
                expected_revision: "0".into(),
                manifest: m,
            },
        )
        .unwrap();
    let view = f
        .workspace
        .view(
            &f.subject,
            "1.21.4",
            &super::super::PackReleases::open(&f.db).unwrap(),
        )
        .unwrap();
    assert!(view.minecraft_drift && view.pending_publication && view.safety_snapshot_recommended);
    assert_eq!(
        f.workspace
            .write(
                &f.subject,
                "1.21.4",
                "stale-mc",
                &DraftWrite {
                    expected_revision: "1".into(),
                    manifest: view.draft.manifest
                }
            )
            .unwrap_err()
            .code(),
        "pack_release_revision_mismatch"
    );
}
#[test]
fn authorization_is_rechecked_before_receipt_replay() {
    let f = Fixture::new();
    let input = DraftWrite {
        expected_revision: "0".into(),
        manifest: f.manifest(),
    };
    f.workspace
        .write(&f.subject, "1.21.1", "save", &input)
        .unwrap();
    f.db.write(|c| {
        c.execute(
            "UPDATE devices SET revoked_at=? WHERE id=?",
            params![f.db.clock().millis(), f.subject.device.as_str()],
        )?;
        Ok(())
    })
    .unwrap();
    assert_eq!(
        f.workspace
            .write(&f.subject, "1.21.1", "save", &input)
            .unwrap_err()
            .code(),
        "forbidden"
    );
}
#[test]
fn unsafe_paths_collisions_and_invalid_loader_metadata_refuse_without_advancing() {
    let f = Fixture::new();
    let mut m = f.manifest();
    m.loader_version = Some("fabric-on-none".into());
    assert!(
        f.workspace
            .write(
                &f.subject,
                "1.21.1",
                "bad",
                &DraftWrite {
                    expected_revision: "0".into(),
                    manifest: m
                }
            )
            .is_err()
    );
    let mut m = f.manifest();
    m.entries.push(PackEntry {
        id: "escape".into(),
        kind: EntryKind::Mod,
        path: "mods/../server.jar".into(),
        required: true,
        environment: Environment {
            client: Side::Required,
            server: Side::Required,
        },
        source: EntrySource::Manual {
            reason: "unknown".into(),
        },
    });
    assert!(validate_manifest(&m).is_err());
    assert_eq!(
        f.workspace.read(&f.subject, "1.21.1").unwrap().revision,
        "0"
    );
}
