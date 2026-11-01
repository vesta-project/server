use super::*;
use std::sync::Arc;
use vesta_state::{Clock, StateDirectory};
struct TestClock;
#[test]
fn server_pack_install_applies_the_selected_override_once() {
    for legacy in [false, true] {
        let f = Fixture::new();
        let game = f.temp.path().join("game");
        std::fs::create_dir_all(game.join("config")).unwrap();
        std::fs::write(game.join("config/setting file.json"), b"local").unwrap();
        let files = crate::files::FileBrowser::open(f.db.clone(), &game, &[]).unwrap();
        let releases = super::super::PackReleases::open(&f.db).unwrap();
        let mut manifest = f.manifest();
        manifest.loader = ClientLoader::Fabric;
        manifest.loader_version = Some("0.16.10".into());
        f.workspace
            .write(
                &f.subject,
                "1.21.1",
                "runtime",
                &DraftWrite {
                    expected_revision: "0".into(),
                    manifest,
                },
            )
            .unwrap();
        for (index, (root, body)) in [
            ("overrides", "shared"),
            ("server-overrides", "server"),
            ("client-overrides", "client"),
        ]
        .iter()
        .enumerate()
        {
            f.workspace
                .file_mutate(
                    &f.subject,
                    "1.21.1",
                    &format!("override-{index}"),
                    &FileMutation {
                        expected_revision: (index + 1).to_string(),
                        change: FileOperation::Write {
                            root: (*root).into(),
                            path: "config/setting file.json".into(),
                            content_base64: STANDARD.encode(body),
                        },
                    },
                )
                .unwrap();
        }
        let plan = f
            .workspace
            .install_plan(
                &f.subject,
                "1.21.1",
                Some("fabric"),
                &PackInstallRequest {
                    source: InstallSource::Draft {
                        revision: "4".into(),
                    },
                },
                &files,
                &releases,
                &crate::content::UreqProviderClient::new(None),
            )
            .unwrap();
        assert_eq!(plan.files.len(), 1);
        assert_eq!(plan.files[0].path, "config/setting file.json");
        assert_eq!(plan.files[0].sha256, digest(b"server"));
        assert_eq!(
            plan.files[0].expected_sha256.as_deref(),
            Some(digest(b"local").as_str())
        );
        assert!(plan.files[0].conflict.is_some());
        assert!(f.workspace.server_link(&f.subject).unwrap().is_none());
        assert_eq!(
            std::fs::read(game.join("config/setting file.json")).unwrap(),
            b"local"
        );
        let apply = PackInstallApply {
                acknowledge_warnings: true,
            acknowledge_software_switch: false,
            plan_id: plan.plan_id,
            confirmation_token: plan.confirmation_token,
            choices: vec![InstallChoice {
                path: "config/setting file.json".into(),
                choice: "replace".into(),
            }],
            set_as_modpack: false,
        };
        let stored: String =
            f.db.read(|c| {
                c.query_row(
                    "SELECT body FROM pack_install_plans WHERE id=?",
                    [&apply.plan_id],
                    |row| row.get(0),
                )
                .map_err(Into::into)
            })
            .unwrap();
        let mut stored: serde_json::Value = serde_json::from_str(&stored).unwrap();
        assert!(stored["payloads"][0]["sha256"].is_string());
        assert!(stored.get("bytes").is_none());
        assert!(
            stored.get("blobs").is_none(),
            "unused blob copies should not be retained"
        );
        if legacy {
            let store = crate::packs::payload::Store::open(&f.db).unwrap();
            let bytes: Vec<Vec<u8>> =
                serde_json::from_value::<Vec<crate::packs::payload::Payload>>(
                    stored["payloads"].clone(),
                )
                .unwrap()
                .iter()
                .map(|payload| {
                    use std::io::Read;
                    let mut body = Vec::new();
                    store
                        .verify(payload)
                        .unwrap()
                        .read_to_end(&mut body)
                        .unwrap();
                    body
                })
                .collect();
            stored.as_object_mut().unwrap().remove("payloads");
            stored["bytes"] = serde_json::json!(bytes);
            if let Some(archive) = stored["client_archive"].as_str() {
                stored["client_archive"] = serde_json::json!(STANDARD.decode(archive).unwrap());
            }
            stored["blobs"] = serde_json::json!([[0, 255]]);
            f.db.write(|c| {
                c.execute(
                    "UPDATE pack_install_plans SET body=? WHERE id=?",
                    params![serde_json::to_string(&stored).unwrap(), apply.plan_id],
                )?;
                Ok(())
            })
            .unwrap();
        }
        let reopened = PackWorkspace::open(&f.db).unwrap();
        let result = reopened
            .install_apply(
                &f.subject,
                "1.21.1",
                Some("fabric"),
                "apply",
                &apply,
                &files,
            )
            .unwrap();
        assert_eq!(result.state, "installed");
        assert_eq!(result.pending_ids.len(), 1);
        let replay = f
            .workspace
            .install_apply(
                &f.subject,
                "1.21.1",
                Some("fabric"),
                "apply",
                &apply,
                &files,
            )
            .unwrap();
        assert_eq!(result.pending_ids, replay.pending_ids);
        assert_eq!(
            f.db.read(|c| c
                .query_row("SELECT count(*) FROM pack_install_plans", [], |r| r
                    .get::<_, i64>(0))
                .map_err(Into::into))
                .unwrap(),
            0,
            "consumed pack bytes should be owned by staging, not retained review slots"
        );
        assert!(
            reopened
                .install_apply(
                    &f.subject,
                    "1.21.1",
                    Some("fabric"),
                    "different-key",
                    &apply,
                    &files
                )
                .is_err(),
            "consumed token was accepted under a new key"
        );
        // A confirmed stopped-server install has already applied the chosen override.
        assert_eq!(
            std::fs::read(game.join("config/setting file.json")).unwrap(),
            b"server"
        );
    }
}
#[test]
#[ignore = "live CurseForge review; downloads provider-verified files into an owned temporary fixture"]
fn live_echoes_of_the_sift_review_does_not_stage_files() {
    let f = Fixture::new();
    let game = f.temp.path().join("game");
    std::fs::create_dir(&game).unwrap();
    let files = crate::files::FileBrowser::open(f.db.clone(), &game, &[]).unwrap();
    let releases = super::super::PackReleases::open(&f.db).unwrap();
    let plan = f
        .workspace
        .install_plan(
            &f.subject,
            "26.3",
            Some("paper"),
            &PackInstallRequest {
                source: InstallSource::Provider {
                    provider: "curseforge".into(),
                    project_id: "1714524".into(),
                    version_id: "9024763".into(),
                },
            },
            &files,
            &releases,
            &crate::content::UreqProviderClient::configured(),
        )
        .unwrap();
    assert!(!plan.files.is_empty());
    assert!(
        plan.files
            .iter()
            .any(|file| file.path == "mods/mielon's-the-sift-1.1.1-mc26.3-fabric.jar")
    );
    assert_eq!(plan.software, "fabric");
    assert_eq!(plan.loader_version.as_deref(), Some("0.19.5"));
    assert!(
        plan.skipped
            .iter()
            .any(|path| path.starts_with("mods/iris-"))
    );
    assert!(
        plan.skipped
            .iter()
            .any(|path| path.starts_with("mods/sodium-"))
    );
    assert!(plan.software_switch_required);
    eprintln!(
        "Echoes review: {} server files, {} client-only files skipped; Fabric {}",
        plan.files.len(),
        plan.skipped.len(),
        plan.loader_version.as_deref().unwrap()
    );
    assert!(f.workspace.server_link(&f.subject).unwrap().is_none());
    assert!(std::fs::read_dir(&game).unwrap().next().is_none());
    assert_eq!(
        f.db.read(|c| c
            .query_row("SELECT count(*) FROM pack_install_plans", [], |r| r
                .get::<_, i64>(0))
            .map_err(Into::into))
            .unwrap(),
        1
    );
}

fn live_provider_install(provider: &str, project: &str, version: &str, mc: &str) {
    let f = Fixture::new();
    let game = f.temp.path().join("game");
    std::fs::create_dir(&game).unwrap();
    let files = crate::files::FileBrowser::open(f.db.clone(), &game, &[]).unwrap();
    let releases = super::super::PackReleases::open(&f.db).unwrap();
    let started = std::time::Instant::now();
    let mut plan = f
        .workspace
        .install_plan(
            &f.subject,
            mc,
            Some("fabric"),
            &PackInstallRequest {
                source: InstallSource::Provider {
                    provider: provider.into(),
                    project_id: project.into(),
                    version_id: version.into(),
                },
            },
            &files,
            &releases,
            &crate::content::UreqProviderClient::configured(),
        )
        .unwrap();
    eprintln!(
        "{provider} real plan: {:.3}s, {} server files, {} client files skipped",
        started.elapsed().as_secs_f64(),
        plan.files.len(),
        plan.skipped.len()
    );
    assert!(plan.valid && !plan.software_switch_required);
    assert!(!plan.files.is_empty());
    assert!(
        std::fs::read_dir(&game).unwrap().next().is_none(),
        "review modified live files"
    );
    assert!(
        !plan
            .warnings
            .iter()
            .any(|warning| warning.detail.starts_with("unknown_or_optional_server_side:"))
    );
    let first_files: Vec<_> = plan
        .files
        .iter()
        .map(|file| (file.path.clone(), file.sha256.clone()))
        .collect();
    let replan_started = std::time::Instant::now();
    plan = f
        .workspace
        .install_plan(
            &f.subject,
            mc,
            Some("fabric"),
            &PackInstallRequest {
                source: InstallSource::Provider {
                    provider: provider.into(),
                    project_id: project.into(),
                    version_id: version.into(),
                },
            },
            &files,
            &releases,
            &crate::content::UreqProviderClient::configured(),
        )
        .unwrap();
    assert_eq!(
        first_files,
        plan.files
            .iter()
            .map(|file| (file.path.clone(), file.sha256.clone()))
            .collect::<Vec<_>>()
    );
    eprintln!(
        "{provider} verified cached replan: {:.3}s",
        replan_started.elapsed().as_secs_f64()
    );
    let input = PackInstallApply {
                acknowledge_warnings: true,
        acknowledge_software_switch: false,
        plan_id: plan.plan_id.clone(),
        confirmation_token: plan.confirmation_token,
        choices: Vec::new(),
        set_as_modpack: false,
    };
    let outcome = f
        .workspace
        .install_apply(
            &f.subject,
            mc,
            Some("fabric"),
            "real-provider-apply",
            &input,
            &files,
        )
        .unwrap();
    assert_eq!(outcome.pending_ids.len(), plan.files.len());
    let replay = f
        .workspace
        .install_apply(
            &f.subject,
            mc,
            Some("fabric"),
            "real-provider-apply",
            &input,
            &files,
        )
        .unwrap();
    assert_eq!(replay.pending_ids, outcome.pending_ids);
    assert_eq!(
        f.db.read(|c| c
            .query_row("SELECT count(*) FROM pack_install_plans", [], |r| r
                .get::<_, i64>(0))
            .map_err(Into::into))
            .unwrap(),
        0
    );
    files.apply_pending_content().unwrap();
    for file in &plan.files {
        let body =
            std::fs::read(game.join(&file.path)).unwrap_or_else(|e| panic!("{}: {e}", file.path));
        assert_eq!(
            digest(&body),
            file.sha256,
            "installed bytes differ at {}",
            file.path
        );
    }
    let reopened = PackWorkspace::open(&f.db).unwrap();
    assert!(reopened.server_link(&f.subject).unwrap().is_some());
    assert!(
        releases.current().unwrap().is_none(),
        "server installation published a client pack"
    );
    eprintln!(
        "{provider} real installation verified: {} files, {:.3}s total; no Minecraft launched",
        plan.files.len(),
        started.elapsed().as_secs_f64()
    );
}

#[test]
#[ignore = "real provider download and file effects in a disposable scratch server; no Minecraft"]
fn live_curseforge_pack_is_installed_and_verified() {
    live_provider_install("curseforge", "1714524", "9024763", "26.3");
}

#[test]
#[ignore = "real provider download and file effects in a disposable scratch server; no Minecraft"]
fn live_modrinth_pack_is_installed_and_verified() {
    live_provider_install("modrinth", "cm9Ke4UE", "G2xrUEZf", "1.21.1");
}
#[test]
fn curseforge_plan_requires_server_configuration_without_staging() {
    let f = Fixture::new();
    let game = f.temp.path().join("game");
    std::fs::create_dir(&game).unwrap();
    std::fs::write(game.join("sentinel"), b"unchanged").unwrap();
    let files = crate::files::FileBrowser::open(f.db.clone(), &game, &[]).unwrap();
    let releases = super::super::PackReleases::open(&f.db).unwrap();
    let result = f.workspace.install_plan(
        &f.subject,
        "26.3",
        Some("paper"),
        &PackInstallRequest {
            source: InstallSource::Provider {
                provider: "curseforge".into(),
                project_id: "1".into(),
                version_id: "2".into(),
            },
        },
        &files,
        &releases,
        &crate::content::UreqProviderClient::new(None),
    );
    assert_eq!(
        result.unwrap_err().code(),
        "content_provider_not_configured"
    );
    assert!(f.workspace.server_link(&f.subject).unwrap().is_none());
    assert!(releases.current().unwrap().is_none());
    assert_eq!(std::fs::read(game.join("sentinel")).unwrap(), b"unchanged");
    assert_eq!(
        f.db.read(|c| c
            .query_row("SELECT count(*) FROM pack_install_plans", [], |r| r
                .get::<_, i64>(0))
            .map_err(Into::into))
            .unwrap(),
        0
    );
}
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
    m.entries[0].path = "mods/Foo/a.jar".into();
    let mut alias = m.entries[0].clone();
    alias.id = "alias".into();
    alias.path = "mods/foo/b.jar".into();
    m.entries.push(alias);
    assert!(validate_manifest(&m).is_err());
    assert_eq!(
        f.workspace.read(&f.subject, "1.21.1").unwrap().revision,
        "0"
    );
}
#[test]
fn streamed_archive_member_does_not_require_consent_for_former_buffer_limit() {
    use std::io::{Cursor, Write};
    let f = Fixture::new();
    let game = f.temp.path().join("game");
    std::fs::create_dir_all(&game).unwrap();
    let files = crate::files::FileBrowser::open(f.db.clone(), &game, &[]).unwrap();
    let releases = super::super::PackReleases::open(&f.db).unwrap();
    let providers = crate::content::UreqProviderClient::new(None);
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    zip.start_file("config/large.dat", zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated)).unwrap();
    zip.write_all(&vec![0; super::super::MAX_PINNED_FILE_BYTES + 1]).unwrap();
    let bytes = zip.finish().unwrap().into_inner();
    let draft = f.workspace.import(&f.subject, "1.21.1", "import-large", &ImportRequest::Archive {
        expected_revision: "0".into(), content_base64: STANDARD.encode(bytes),
    }, &releases, &providers).unwrap();
    let plan = f.workspace.install_plan(&f.subject, "1.21.1", Some("vanilla"), &PackInstallRequest {
        source: InstallSource::Draft { revision: draft.revision },
    }, &files, &releases, &providers).unwrap();
    assert!(plan.valid);
    assert!(!plan.warnings.iter().any(|warning| warning.rule.starts_with("legacy_")));
    let apply = PackInstallApply {
        // Unknown file side and unavailable client delivery still need review.
        acknowledge_warnings: true, acknowledge_software_switch: true,
        plan_id: plan.plan_id, confirmation_token: plan.confirmation_token,
        choices: Vec::new(), set_as_modpack: false,
    };
    let pending: i64 = f.db.read(|c| c.query_row("SELECT count(*) FROM server_content_changes", [], |r| r.get(0)).map_err(Into::into)).unwrap();
    assert_eq!(pending, 0);
    let outcome = f.workspace.install_apply(&f.subject, "1.21.1", Some("vanilla"), "apply-large", &apply, &files).unwrap();
    assert_eq!(outcome.pending_ids.len(), 1);
    assert_eq!(outcome.state, "installed");
    assert_eq!(std::fs::metadata(game.join("config/large.dat")).unwrap().len(), (super::super::MAX_PINNED_FILE_BYTES + 1) as u64);
}
