use super::*;
use crate::identity::{
    BuiltinRole,
    management::{Enrolled, session_message},
    minecraft::{MinecraftUuid, ServerUuid, VerifiedMinecraftProfile},
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use ring::{
    rand::SystemRandom,
    signature::{Ed25519KeyPair, KeyPair},
};
use std::sync::Arc;
use vesta_state::{StateDirectory, SystemClock};
#[path = "consumer_tests.rs"]
mod consumer_tests;

const INSTALLATION: &str = "0123456789abcdef0123456789abcdef";
struct Fixture {
    root: tempfile::TempDir,
    db: StateDatabase,
    setup: FirstServerSetup,
}
impl Fixture {
    fn new() -> Self {
        Self::at("state")
    }
    fn at(state: &str) -> Self {
        let root = tempfile::tempdir().unwrap();
        let db = StateDatabase::open(
            StateDirectory::new(root.path().join(state), None).unwrap(),
            Arc::new(SystemClock),
        )
        .unwrap();
        let setup = FirstServerSetup::reserve_local(db.clone(), INSTALLATION).unwrap();
        Self { root, db, setup }
    }
    fn enroll(&self, role: BuiltinRole) -> (Enrolled, Ed25519KeyPair, String) {
        let key = Ed25519KeyPair::from_pkcs8(
            Ed25519KeyPair::generate_pkcs8(&SystemRandom::new())
                .unwrap()
                .as_ref(),
        )
        .unwrap();
        let m = self.setup.management();
        let code = m.issue_local(role, 10).unwrap();
        let profile = VerifiedMinecraftProfile::for_test_enrollment(
            ServerUuid::parse(self.db.server_id()).unwrap(),
            MinecraftUuid::parse(&uuid::Uuid::new_v4().to_string()).unwrap(),
        );
        let device = m
            .enroll_minecraft_with_invitation(
                profile,
                "Owned setup fixture",
                &URL_SAFE_NO_PAD.encode(key.public_key().as_ref()),
                Some(code.secret.expose()),
            )
            .unwrap();
        let session = session(m, &device, &key);
        (device, key, session)
    }
}

fn owned_provisioning(f: &Fixture, source: &std::path::Path) -> super::provisioning::Provisioning {
    super::provisioning::Provisioning::open(
        f.setup.clone(),
        f.root.path(),
        super::provisioning::LocalConfiguration {
            java: std::env::current_exe().unwrap(),
            adoption_jar: Some("server.jar".into()),
            adoption_argfile: None,
            copied_export_confirmed: true,
        },
        Some(source),
        25567,
    )
    .unwrap()
}
fn provision_state(
    p: &super::provisioning::Provisioning,
    token: &str,
    id: &str,
    state: &str,
) -> super::provisioning::Job {
    let until = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let job = p.status(token, id).unwrap();
        if job.state == state {
            return job;
        }
        assert_ne!(job.state, "failed", "{:?}", job.error);
        assert!(std::time::Instant::now() < until, "{}", job.state);
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}
#[test]
fn provisioning_concurrent_apply_is_same_job_and_ready_reopen_rechecks_witnesses() {
    use super::provisioning::{Apply, Request};
    let f = Fixture::at("private/state");
    let (device, key, token) = f.enroll(BuiltinRole::Owner);
    f.setup.claim(&token).unwrap();
    let source = tempfile::tempdir().unwrap();
    std::fs::write(source.path().join("server.jar"), b"inert-jar").unwrap();
    std::fs::create_dir(source.path().join("world")).unwrap();
    std::fs::write(source.path().join("world/level.dat"), b"unchanged-world").unwrap();
    let p = owned_provisioning(&f, source.path());
    let accepted = p
        .prepare(
            &token,
            Request::AdoptCopy {
                name: "Owned".into(),
                minecraft_version: "1.21.1".into(),
            },
        )
        .unwrap();
    let review = provision_state(&p, &token, &accepted.id, "review");
    let gate = Arc::new(std::sync::Barrier::new(3));
    let mut threads = Vec::new();
    for _ in 0..2 {
        let p = p.clone();
        let token = token.clone();
        let id = accepted.id.clone();
        let confirmation = review.confirmation.clone().unwrap();
        let gate = gate.clone();
        threads.push(std::thread::spawn(move || {
            gate.wait();
            p.apply(
                &token,
                Apply {
                    job_id: id,
                    confirmation,
                    idempotency_key: "same-intent".into(),
                    acknowledge_warnings: true,
                    acknowledge_legacy_checksum: false,
                },
            )
        }));
    }
    gate.wait();
    for h in threads {
        assert_eq!(h.join().unwrap().unwrap().id, accepted.id);
    }
    provision_state(&p, &token, &accepted.id, "ready");
    p.shutdown().unwrap();
    drop(p);
    let Fixture { root, db, setup } = f;
    drop(setup);
    db.close().unwrap();
    drop(db);
    let db = StateDatabase::open(
        StateDirectory::new(root.path().join("private/state"), None).unwrap(),
        Arc::new(SystemClock),
    )
    .unwrap();
    let setup = FirstServerSetup::reserve_local(db.clone(), INSTALLATION).unwrap();
    let f = Fixture { root, db, setup };
    let token = session(f.setup.management(), &device, &key);
    let p = owned_provisioning(&f, source.path());
    assert_eq!(p.status(&token, &accepted.id).unwrap().state, "ready");
    let world = f.root.path().join("servers/first/game/world/level.dat");
    std::fs::write(&world, b"substituted-world").unwrap();
    let calls = std::cell::Cell::new(0);
    assert_eq!(
        p.publish_ready(|_, _, _, _| {
            calls.set(calls.get() + 1);
            Ok(())
        }),
        Err(PermissionError::PreconditionFailed)
    );
    assert_eq!(calls.get(), 0);
    std::fs::write(&world, b"unchanged-world").unwrap();
    assert_eq!(
        p.publish_ready(|_, _, _, _| Err(PermissionError::Unavailable)),
        Err(PermissionError::Unavailable)
    );
    assert_eq!(p.status(&token, &accepted.id).unwrap().state, "ready");
    assert!(p.publish_ready(|_, _, _, _| Ok(())).unwrap());
    assert!(p.publish_ready(|_, _, _, _| Ok(())).unwrap());
    assert_eq!(p.status(&token, &accepted.id).unwrap().state, "complete");
    assert!(f.setup.audit.verify_chain().unwrap().valid);
    assert_eq!(
        std::fs::read(source.path().join("world/level.dat")).unwrap(),
        b"unchanged-world"
    );
    p.shutdown().unwrap();
}
#[test]
fn provisioning_rejects_foreign_composition_and_revoked_actor_before_copy() {
    use super::provisioning::{Apply, LocalConfiguration, Provisioning, Request};
    let f = Fixture::at("private/state");
    let (device, _, token) = f.enroll(BuiltinRole::Owner);
    f.setup.claim(&token).unwrap();
    let source = tempfile::tempdir().unwrap();
    std::fs::write(source.path().join("server.jar"), b"inert").unwrap();
    let foreign = tempfile::tempdir().unwrap();
    assert!(
        Provisioning::open(
            f.setup.clone(),
            foreign.path(),
            LocalConfiguration {
                java: std::env::current_exe().unwrap(),
                adoption_jar: Some("server.jar".into()),
                adoption_argfile: None,
                copied_export_confirmed: true
            },
            Some(source.path()),
            25567
        )
        .is_err()
    );
    assert!(
        Provisioning::open(
            f.setup.clone(),
            f.root.path(),
            LocalConfiguration {
                java: std::env::current_exe().unwrap(),
                adoption_jar: Some("server.jar".into()),
                adoption_argfile: None,
                copied_export_confirmed: false
            },
            Some(source.path()),
            25567
        )
        .is_err()
    );
    let p = owned_provisioning(&f, source.path());
    let job = p
        .prepare(
            &token,
            Request::AdoptCopy {
                name: "Owned".into(),
                minecraft_version: "1.21.1".into(),
            },
        )
        .unwrap();
    let review = provision_state(&p, &token, &job.id, "review");
    f.db.write(|c| {
        c.execute(
            "UPDATE devices SET revoked_at=1 WHERE id=?",
            [&device.device_id],
        )?;
        Ok(())
    })
    .unwrap();
    assert!(
        p.apply(
            &token,
            Apply {
                job_id: job.id,
                confirmation: review.confirmation.unwrap(),
                idempotency_key: "revoked".into(),
                acknowledge_warnings: true,
                acknowledge_legacy_checksum: false
            }
        )
        .is_err()
    );
    assert!(!f.root.path().join("servers").exists());
    assert_eq!(
        std::fs::read(source.path().join("server.jar")).unwrap(),
        b"inert"
    );
    p.shutdown().unwrap();
}
fn session(m: &ManagementAccess, device: &Enrolled, key: &Ed25519KeyPair) -> String {
    let nonce = m.challenge(&device.device_id).unwrap();
    let signed = key.sign(&session_message(m.server_id(), nonce.expose()));
    m.session(
        &device.device_id,
        nonce.expose(),
        &URL_SAFE_NO_PAD.encode(signed.as_ref()),
    )
    .unwrap()
    .secret
    .expose()
    .to_owned()
}

#[test]
fn first_account_claim_is_idempotent_and_has_no_machine_authority() {
    let f = Fixture::new();
    let (_, _, token) = f.enroll(BuiltinRole::Owner);
    let before = f.setup.audit.page(0, 100).unwrap().len();
    let result = f.setup.claim(&token).unwrap();
    assert!(result.claimed);
    assert_eq!(result.server_id, f.db.server_id());
    assert_eq!(f.setup.claim(&token).unwrap(), result);
    assert_eq!(f.setup.claimed_status(&token).unwrap(), result);
    assert_eq!(f.setup.audit.page(0, 100).unwrap().len(), before + 1);
    let value = serde_json::to_value(&result).unwrap();
    assert_eq!(value.as_object().unwrap().len(), 3);
    assert!(f.setup.with_owner(&token, |_, _, _| Ok(())).is_ok());
    assert!(
        FirstServerSetup::reserve_local(f.db.clone(), "fedcba9876543210fedcba9876543210").is_err()
    );
    assert!(f.setup.audit.verify_chain().unwrap().valid);
}

#[test]
fn other_accounts_lesser_roles_and_foreign_sessions_cannot_take_over() {
    let f = Fixture::new();
    let (_, _, first) = f.enroll(BuiltinRole::Owner);
    let (_, _, admin) = f.enroll(BuiltinRole::Administrator);
    assert_eq!(f.setup.claim(&admin), Err(PermissionError::Forbidden));
    f.setup.claim(&first).unwrap();
    let (_, _, other) = f.enroll(BuiltinRole::Owner);
    assert_eq!(f.setup.claim(&other), Err(PermissionError::Conflict));
    assert_eq!(
        f.setup.with_owner(&other, |_, _, _| Ok(())),
        Err(PermissionError::Forbidden)
    );
    let foreign = Fixture::new();
    let (_, _, token) = foreign.enroll(BuiltinRole::Owner);
    assert_eq!(f.setup.claim(&token), Err(PermissionError::SessionInvalid));
}

#[test]
fn revocation_or_account_unlink_refuses_execution_before_effect() {
    let f = Fixture::new();
    let (device, _, token) = f.enroll(BuiltinRole::Owner);
    f.setup.claim(&token).unwrap();
    f.db.write(|c| {
        c.execute(
            "UPDATE management_device_keys SET linked_minecraft_uuid=NULL WHERE device_id=?",
            [&device.device_id],
        )?;
        Ok(())
    })
    .unwrap();
    let effects = std::cell::Cell::new(0);
    assert_eq!(
        f.setup.with_owner(&token, |_, _, _| {
            effects.set(1);
            Ok(())
        }),
        Err(PermissionError::SessionInvalid)
    );
    assert_eq!(effects.get(), 0);
}

#[test]
fn claim_audit_failure_rolls_back_owner_binding() {
    let f = Fixture::new();
    let (_, _, token) = f.enroll(BuiltinRole::Owner);
    f.db.write(|c|{c.execute_batch("CREATE TRIGGER setup_audit_failure BEFORE INSERT ON audit_events BEGIN SELECT RAISE(ABORT,'owned injected audit failure'); END")?;Ok(())}).unwrap();
    assert_eq!(f.setup.claim(&token), Err(PermissionError::Unavailable));
    assert!(
        f.db.read(|c| Ok(c.query_row(
            "SELECT owner_principal IS NULL FROM first_server_setup",
            [],
            |r| r.get::<_, bool>(0)
        )?))
        .unwrap()
    );
    f.db.write(|c| {
        c.execute_batch("DROP TRIGGER setup_audit_failure")?;
        Ok(())
    })
    .unwrap();
    assert!(f.setup.claim(&token).unwrap().claimed);
}

#[test]
fn real_database_reopen_retains_claim_but_requires_new_session() {
    let Fixture { root, db, setup } = Fixture::new();
    let f = Fixture { root, db, setup };
    let (device, key, token) = f.enroll(BuiltinRole::Owner);
    let expected = f.setup.claim(&token).unwrap();
    let Fixture { root, db, setup } = f;
    drop(setup);
    db.close().unwrap();
    drop(db);
    let db = StateDatabase::open(
        StateDirectory::new(root.path().join("state"), None).unwrap(),
        Arc::new(SystemClock),
    )
    .unwrap();
    let setup = FirstServerSetup::reserve_local(db.clone(), INSTALLATION).unwrap();
    assert_eq!(setup.claim(&token), Err(PermissionError::SessionInvalid));
    let renewed = session(setup.management(), &device, &key);
    assert_eq!(setup.claim(&renewed).unwrap(), expected);
}

#[test]
fn concurrent_account_claims_choose_one_owner_and_replay_cannot_replace_it() {
    let f = Fixture::new();
    let (_, _, a) = f.enroll(BuiltinRole::Owner);
    let (_, _, b) = f.enroll(BuiltinRole::Owner);
    let barrier = Arc::new(std::sync::Barrier::new(3));
    let mut threads = Vec::new();
    for token in [a.clone(), b.clone()] {
        let setup = f.setup.clone();
        let gate = barrier.clone();
        threads.push(std::thread::spawn(move || {
            gate.wait();
            setup.claim(&token)
        }));
    }
    barrier.wait();
    let results = threads
        .into_iter()
        .map(|t| t.join().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter(|r| **r == Err(PermissionError::Conflict))
            .count(),
        1
    );
    assert_eq!(
        f.setup
            .audit
            .page(0, 100)
            .unwrap()
            .iter()
            .filter(|r| r.action == "setup.first_server.claim")
            .count(),
        1
    );
}

#[test]
fn revoked_session_and_removed_owner_role_do_not_admit_setup_mutations() {
    let f = Fixture::new();
    let (device, key, token) = f.enroll(BuiltinRole::Owner);
    f.setup.claim(&token).unwrap();
    assert!(f.setup.management().revoke_session(&token).unwrap());
    assert_eq!(
        f.setup.claimed_status(&token),
        Err(PermissionError::SessionInvalid)
    );
    let next = session(f.setup.management(), &device, &key);
    f.db.write(|c| {
        c.execute(
            "UPDATE role_assignments SET revoked_at=1 WHERE principal_id=? AND role_id='owner'",
            [&device.principal_id],
        )?;
        Ok(())
    })
    .unwrap();
    assert_eq!(
        f.setup.claimed_status(&next),
        Err(PermissionError::Forbidden)
    );
}
#[test]
fn copied_export_preserves_source_witnesses_and_resumes_only_matching_effects() {
    use crate::files::{FileBrowser, FileError, copied_export::CopiedExport};
    let f = Fixture::new();
    let (device, _, token) = f.enroll(BuiltinRole::Owner);
    f.setup.claim(&token).unwrap();
    let actor = f.setup.management().authenticate(&token).unwrap();
    assert_eq!(actor.device.as_str(), device.device_id);
    let source = f.root.path().join("copied-export");
    let game = f.root.path().join("new-game");
    std::fs::create_dir_all(source.join("world/datapacks")).unwrap();
    std::fs::create_dir_all(source.join("config")).unwrap();
    std::fs::create_dir(&game).unwrap();
    for (name, bytes) in [
        ("world/level.dat", b"world-witness".as_slice()),
        ("world/datapacks/pack.zip", b"datapack-witness"),
        ("config/custom.toml", b"custom-witness"),
        ("eula.txt", b"eula=false\n"),
    ] {
        std::fs::write(source.join(name), bytes).unwrap();
    }
    let export = CopiedExport::open(&source, &[f.db.directory().path(), &game]).unwrap();
    let plan = export.snapshot().unwrap();
    let before = serde_json::to_vec(&plan).unwrap();
    let target = FileBrowser::open(f.db.clone(), &game, &[f.db.directory().path()]).unwrap();
    let mut steps = 0;
    assert!(
        export
            .copy_into(&plan, &target, &actor, || {
                steps += 1;
                if steps == 4 {
                    Err(FileError::Unavailable)
                } else {
                    Ok(())
                }
            })
            .is_err()
    );
    export.copy_into(&plan, &target, &actor, || Ok(())).unwrap();
    export.copy_into(&plan, &target, &actor, || Ok(())).unwrap();
    assert_eq!(
        serde_json::to_vec(&export.snapshot().unwrap()).unwrap(),
        before
    );
    for entry in &plan.entries {
        if !entry.directory {
            assert_eq!(
                std::fs::read(source.join(&entry.path)).unwrap(),
                std::fs::read(game.join(&entry.path)).unwrap()
            );
        }
    }
    assert_eq!(
        std::fs::read(game.join("eula.txt")).unwrap(),
        b"eula=false\n"
    );
    std::fs::write(game.join("unexpected-owner-file"), b"keep").unwrap();
    assert!(export.copy_into(&plan, &target, &actor, || Ok(())).is_err());
    assert_eq!(
        std::fs::read(game.join("unexpected-owner-file")).unwrap(),
        b"keep"
    );
}

#[test]
fn copied_export_refuses_mutated_source_links_and_wrapper_material_without_source_effects() {
    use crate::files::{FileBrowser, copied_export::CopiedExport};
    let f = Fixture::new();
    let (_, _, token) = f.enroll(BuiltinRole::Owner);
    f.setup.claim(&token).unwrap();
    let actor = f.setup.management().authenticate(&token).unwrap();
    let source = f.root.path().join("copy");
    let game = f.root.path().join("destination");
    std::fs::create_dir(&source).unwrap();
    std::fs::create_dir(&game).unwrap();
    std::fs::write(source.join("server.jar"), b"inert-reviewed-jar").unwrap();
    let export = CopiedExport::open(&source, &[f.db.directory().path(), &game]).unwrap();
    let plan = export.snapshot().unwrap();
    let target = FileBrowser::open(f.db.clone(), &game, &[f.db.directory().path()]).unwrap();
    std::fs::write(source.join("server.jar"), b"changed").unwrap();
    assert!(export.copy_into(&plan, &target, &actor, || Ok(())).is_err());
    assert_eq!(std::fs::read_dir(&game).unwrap().count(), 0);
    std::fs::write(source.join("vesta-state.db"), b"private-wrapper-material").unwrap();
    assert!(export.snapshot().is_err());
    std::fs::remove_file(source.join("vesta-state.db")).unwrap();
    #[cfg(windows)]
    std::os::windows::fs::symlink_file(f.root.path().join("outside"), source.join("escape"))
        .unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(f.root.path().join("outside"), source.join("escape")).unwrap();
    assert!(export.snapshot().is_err());
    assert_eq!(
        std::fs::read(source.join("server.jar")).unwrap(),
        b"changed"
    );
}

#[test]
fn copied_export_retires_audited_effects_without_exhausting_the_file_journal() {
    use crate::files::{FileBrowser, copied_export::CopiedExport};
    let f = Fixture::new();
    let (_, _, token) = f.enroll(BuiltinRole::Owner);
    f.setup.claim(&token).unwrap();
    let actor = f.setup.management().authenticate(&token).unwrap();
    let source = f.root.path().join("many-source");
    let game = f.root.path().join("many-destination");
    std::fs::create_dir(&source).unwrap();
    std::fs::create_dir(&game).unwrap();
    for index in 0..(crate::files::MAX_FILE_INTENTS + 1) {
        std::fs::write(
            source.join(format!("file-{index:04}")),
            b"bounded-owned-fixture",
        )
        .unwrap();
    }
    let export = CopiedExport::open(&source, &[f.db.directory().path(), &game]).unwrap();
    let snapshot = export.snapshot().unwrap();
    let destination = FileBrowser::open(f.db.clone(), &game, &[f.db.directory().path()]).unwrap();
    export
        .copy_into(&snapshot, &destination, &actor, || Ok(()))
        .unwrap();
    assert_eq!(
        std::fs::read_dir(&game).unwrap().count(),
        crate::files::MAX_FILE_INTENTS + 1
    );
    assert_eq!(
        f.db.read(|c| c
            .query_row("SELECT count(*) FROM file_intents", [], |r| r
                .get::<_, i64>(0))
            .map_err(Into::into))
            .unwrap(),
        0
    );
    assert!(f.setup.audit.verify_chain().unwrap().valid);
}

#[test]
fn copied_argfile_adoption_uses_explicit_local_selection_and_shared_launch_validation() {
    use super::provisioning::{Apply, LocalConfiguration, Provisioning, Request};
    let f = Fixture::at("private/state");
    let (_, _, token) = f.enroll(BuiltinRole::Owner);
    f.setup.claim(&token).unwrap();
    let source = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(source.path().join("libraries/forge")).unwrap();
    let argument_bytes = b"-cp libraries/owned.jar owned.Main\n";
    std::fs::write(
        source.path().join("libraries/forge/args.txt"),
        argument_bytes,
    )
    .unwrap();
    std::fs::write(source.path().join("libraries/owned.jar"), b"inert").unwrap();
    let p = Provisioning::open(
        f.setup.clone(),
        f.root.path(),
        LocalConfiguration {
            java: std::env::current_exe().unwrap(),
            adoption_jar: None,
            adoption_argfile: Some("libraries/forge/args.txt".into()),
            copied_export_confirmed: true,
        },
        Some(source.path()),
        25567,
    )
    .unwrap();
    let job = p
        .prepare(
            &token,
            Request::AdoptCopy {
                name: "Owned explicit argfile".into(),
                minecraft_version: "1.21.1".into(),
            },
        )
        .unwrap();
    let review = provision_state(&p, &token, &job.id, "review");
    p.apply(
        &token,
        Apply {
            job_id: job.id.clone(),
            confirmation: review.confirmation.unwrap(),
            idempotency_key: "owned-argfile".into(),
            acknowledge_warnings: true,
            acknowledge_legacy_checksum: false,
        },
    )
    .unwrap();
    provision_state(&p, &token, &job.id, "ready");
    assert!(
        p.publish_ready(|_, _, _, document| {
            let parsed = vesta_game_runtime::launch_config::LaunchConfiguration::parse_toml(
                document.as_bytes(),
                f.root.path(),
            )
            .unwrap();
            assert!(parsed.into_launch_spec(None).is_ok());
            let value: toml::Value = toml::from_str(document).unwrap();
            let args = value["launch"]["args"].as_array().unwrap();
            assert!(args[0].as_str().unwrap().starts_with('@'));
            assert!(args[0].as_str().unwrap().ends_with("args.txt"));
            assert_eq!(args[1].as_str(), Some("nogui"));
            Ok(())
        })
        .unwrap()
    );
    p.shutdown().unwrap();
    assert_eq!(
        std::fs::read(source.path().join("libraries/forge/args.txt")).unwrap(),
        argument_bytes
    );
    assert_eq!(
        std::fs::read(
            f.root
                .path()
                .join("servers/first/game/libraries/forge/args.txt")
        )
        .unwrap(),
        argument_bytes
    );
}
