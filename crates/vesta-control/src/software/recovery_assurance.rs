//! No installer/network/process launches: fixture state + fake stopped hooks only.
use super::*;
fn owned() -> Fixture {
    let f = Fixture::new();
    let root = f._temp.path().canonicalize().unwrap();
    assert!(f.game.canonicalize().unwrap().starts_with(&root));
    assert!(
        f.db.directory()
            .path()
            .canonicalize()
            .unwrap()
            .starts_with(&root)
    );
    std::fs::write(root.join("recovery-assurance-owned"), b"fixture-only").unwrap();
    std::fs::create_dir_all(f.game.join("config")).unwrap();
    std::fs::write(
        f.game.join("config/managed-recovery.properties"),
        b"managed witness",
    )
    .unwrap();
    f
}
fn witnesses(f: &Fixture) {
    f.unchanged();
    assert_eq!(
        std::fs::read(f.game.join("config/managed-recovery.properties")).unwrap(),
        b"managed witness"
    );
}
fn reload(f: &mut Fixture) {
    let directory = f.db.directory().clone();
    f.db.close().unwrap();
    f.db = StateDatabase::open(directory, Arc::new(SystemClock)).unwrap();
}
fn scalar(f: &Fixture, sql: &str) -> i64 {
    f.db.read(|c| Ok(c.query_row(sql, [], |r| r.get(0))?))
        .unwrap()
}
fn completion(h: &SoftwareHandle) -> std::sync::mpsc::Receiver<()> {
    let (tx, rx) = std::sync::mpsc::channel();
    *h.inner.completion_probe.lock().unwrap() = Some(tx);
    rx
}
#[test]
fn recovery_assurance_persisted_stale_jobs_are_not_enqueued_on_three_reopens() {
    let mut f = owned();
    let service = f.open();
    drop(service);
    f.db.write(|c|{for (id,state) in [("queued-fixture","queued"),("staging-fixture","staging"),("activating-fixture","activating")]{c.execute("INSERT INTO software_jobs(id,kind,state,principal,device,idempotency_key,request_hash,expected_revision,created_at_ms,updated_at_ms) VALUES(?,'rollback',?,?,?,?,?,0,1,1)",params![id,state,PRINCIPAL,DEVICE,id,hex::encode(sha2::Sha256::digest(id.as_bytes()))])?;}Ok(())}).unwrap();
    for _ in 0..3 {
        reload(&mut f);
        let service = f.open();
        let h = service.handle();
        let jobs = h.jobs(&owner()).unwrap();
        assert_eq!(jobs.len(), 3);
        for j in jobs {
            assert_eq!(
                j.state,
                if j.id == "activating-fixture" {
                    "recovery_required"
                } else {
                    "failed"
                }
            );
            assert_eq!(
                j.error.as_deref(),
                Some(if j.id == "activating-fixture" {
                    "software_activation_interrupted"
                } else {
                    "software_staging_interrupted"
                })
            );
        }
        assert!(h.status(&owner()).unwrap().recovery_required);
        assert!(f.native.blocked.load(Ordering::SeqCst));
        assert_eq!(f.native.calls.load(Ordering::SeqCst), 0);
        witnesses(&f);
        drop(h);
        drop(service);
    }
}
#[test]
fn recovery_assurance_lost_waiter_same_intent_and_three_reopens_do_not_duplicate_activation() {
    let mut f = owned();
    let service = f.open();
    let h = service.handle();
    f.previous("previous launch");
    let p = h.prepare_rollback(&owner(), 0).unwrap();
    let done = completion(&h);
    let (arrived_tx, arrived_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    *f.native.activation_action.lock().unwrap() = Some(Box::new(move || {
        arrived_tx.send(()).unwrap();
        release_rx.recv_timeout(Duration::from_secs(10)).unwrap();
    }));
    let first = h
        .rollback(&owner(), request(&p, "recovery-waiter-key"))
        .unwrap();
    let id = first.id.clone();
    drop(first);
    drop(h);
    arrived_rx.recv_timeout(Duration::from_secs(10)).unwrap();
    let reconnect = service.handle();
    let replay = reconnect
        .rollback(&owner(), request(&p, "recovery-waiter-key"))
        .unwrap();
    assert_eq!(replay.id, id);
    assert_eq!(replay.state, "activating");
    assert_eq!(scalar(&f, "SELECT count(*) FROM software_jobs"), 1);
    release_tx.send(()).unwrap();
    done.recv_timeout(Duration::from_secs(10)).unwrap();
    assert_eq!(reconnect.jobs(&owner()).unwrap()[0].state, "succeeded");
    drop(reconnect);
    drop(service);
    for _ in 0..3 {
        reload(&mut f);
        let service = f.open();
        let h = service.handle();
        let replay = h
            .rollback(&owner(), request(&p, "recovery-waiter-key"))
            .unwrap();
        assert_eq!(replay.id, id);
        assert_eq!(replay.state, "succeeded");
        assert_eq!(scalar(&f, "SELECT count(*) FROM software_jobs"), 1);
        assert_eq!(
            scalar(
                &f,
                "SELECT count(*) FROM audit_events WHERE action='software.complete'"
            ),
            1
        );
        assert_eq!(f.native.calls.load(Ordering::SeqCst), 1);
        assert!(!f.native.blocked.load(Ordering::SeqCst));
        assert_eq!(h.status(&owner()).unwrap().revision, 1);
        witnesses(&f);
        drop(h);
        drop(service);
    }
}
#[test]
fn recovery_assurance_revocation_after_fake_activation_keeps_recovery_and_previous_on_reopen() {
    let mut f = owned();
    let service = f.open();
    let h = service.handle();
    f.previous("previous launch");
    let p = h.prepare_rollback(&owner(), 0).unwrap();
    let done = completion(&h);
    let db = f.db.clone();
    *f.native.activation_action.lock().unwrap() = Some(Box::new(move || {
        db.read(|c| {
            let blocked: bool = c.query_row(
                "SELECT recovery_required FROM software_state WHERE singleton=1",
                [],
                |r| r.get(0),
            )?;
            assert!(blocked, "recovery durable before fake effect");
            let state: String = c.query_row("SELECT state FROM software_jobs", [], |r| r.get(0))?;
            assert_eq!(state, "activating");
            Ok(())
        })
        .unwrap();
        crate::identity::management::ManagementAccess::new(db)
            .unwrap()
            .revoke_local(DEVICE)
            .unwrap();
    }));
    let job = h
        .rollback(&owner(), request(&p, "recovery-revoked-key"))
        .unwrap();
    done.recv_timeout(Duration::from_secs(10)).unwrap();
    assert!(matches!(
        h.rollback(&owner(), request(&p, "recovery-revoked-key")),
        Err(SoftwareError::Forbidden)
    ));
    drop(h);
    drop(service);
    for _ in 0..3 {
        reload(&mut f);
        let service = f.open();
        let (state, recovery, previous): (String, bool, bool) =
            f.db.read(|c| {
                Ok((
                    c.query_row(
                        "SELECT state FROM software_jobs WHERE id=?",
                        [&job.id],
                        |r| r.get(0),
                    )?,
                    c.query_row(
                        "SELECT recovery_required FROM software_state WHERE singleton=1",
                        [],
                        |r| r.get(0),
                    )?,
                    c.query_row(
                        "SELECT previous_launch IS NOT NULL FROM software_state WHERE singleton=1",
                        [],
                        |r| r.get(0),
                    )?,
                ))
            })
            .unwrap();
        assert_eq!(state, "recovery_required");
        assert!(recovery && previous);
        assert_eq!(f.native.calls.load(Ordering::SeqCst), 1);
        assert!(f.native.blocked.load(Ordering::SeqCst));
        assert_eq!(
            scalar(
                &f,
                "SELECT count(*) FROM audit_events WHERE action='software.complete' AND outcome='succeeded'"
            ),
            0
        );
        witnesses(&f);
        drop(service);
    }
}
#[test]
fn recovery_assurance_failed_fake_activation_is_not_resumed_by_same_intent_or_reopen() {
    let mut f = owned();
    let service = f.open();
    let h = service.handle();
    f.previous("previous launch");
    let p = h.prepare_rollback(&owner(), 0).unwrap();
    let done = completion(&h);
    f.native.fail_after_write.store(true, Ordering::SeqCst);
    let job = h
        .rollback(&owner(), request(&p, "recovery-failed-key"))
        .unwrap();
    done.recv_timeout(Duration::from_secs(10)).unwrap();
    assert_eq!(h.jobs(&owner()).unwrap()[0].state, "recovery_required");
    assert_eq!(
        h.rollback(&owner(), request(&p, "recovery-failed-key"))
            .unwrap()
            .id,
        job.id
    );
    drop(h);
    drop(service);
    for _ in 0..3 {
        reload(&mut f);
        let service = f.open();
        let h = service.handle();
        let replay = h
            .rollback(&owner(), request(&p, "recovery-failed-key"))
            .unwrap();
        assert_eq!(replay.id, job.id);
        assert_eq!(replay.state, "recovery_required");
        assert!(h.status(&owner()).unwrap().recovery_required);
        assert_eq!(f.native.calls.load(Ordering::SeqCst), 1);
        assert!(f.native.blocked.load(Ordering::SeqCst));
        assert_eq!(scalar(&f, "SELECT count(*) FROM software_jobs"), 1);
        assert_eq!(
            scalar(
                &f,
                "SELECT count(*) FROM audit_events WHERE action='software.complete' AND outcome='succeeded'"
            ),
            0
        );
        witnesses(&f);
        drop(h);
        drop(service);
    }
}
#[test]
fn recovery_assurance_activation_completion_commit_and_audit_faults_never_publish_success() {
    for commit in [false, true] {
        let mut f = owned();
        let service = f.open();
        let h = service.handle();
        f.previous("previous launch");
        let p = h.prepare_rollback(&owner(), 0).unwrap();
        let done = completion(&h);
        f.db.write(|c|{c.execute_batch(if commit{"CREATE TABLE completion_failure(principal TEXT REFERENCES principals(id) DEFERRABLE INITIALLY DEFERRED); CREATE TRIGGER fail_completion AFTER INSERT ON audit_events WHEN NEW.action='software.complete' AND NEW.outcome='succeeded' BEGIN INSERT INTO completion_failure VALUES('missing'); END;"}else{"CREATE TRIGGER fail_completion BEFORE INSERT ON audit_events WHEN NEW.action='software.complete' AND NEW.outcome='succeeded' BEGIN SELECT RAISE(ABORT,'owned-fixture'); END;"})?;Ok(())}).unwrap();
        let job = h
            .rollback(&owner(), request(&p, "completion-fault-key"))
            .unwrap();
        done.recv_timeout(Duration::from_secs(10)).unwrap();
        assert_eq!(h.jobs(&owner()).unwrap()[0].state, "recovery_required");
        drop(h);
        drop(service);
        for _ in 0..3 {
            reload(&mut f);
            let service = f.open();
            let h = service.handle();
            assert_eq!(
                h.rollback(&owner(), request(&p, "completion-fault-key"))
                    .unwrap()
                    .id,
                job.id
            );
            assert_eq!(h.jobs(&owner()).unwrap()[0].state, "recovery_required");
            assert_eq!(h.status(&owner()).unwrap().revision, 0);
            assert_eq!(
                scalar(
                    &f,
                    "SELECT count(*) FROM audit_events WHERE action='software.complete' AND outcome='succeeded'"
                ),
                0
            );
            assert_eq!(
                scalar(
                    &f,
                    "SELECT count(*) FROM audit_events WHERE action='software.complete' AND outcome='recovery_required'"
                ),
                1
            );
            assert_eq!(f.native.calls.load(Ordering::SeqCst), 1);
            assert!(f.native.blocked.load(Ordering::SeqCst));
            witnesses(&f);
            drop(h);
            drop(service);
        }
    }
}
#[test]
fn recovery_assurance_stopped_activation_does_not_clear_unresolved_content_start_barrier() {
    let mut f = owned();
    let service = f.open();
    let h = service.handle();
    let files = crate::files::FileBrowser::open(f.db.clone(), &f.game, &[]).unwrap();
    std::fs::create_dir_all(f.game.join("config")).unwrap();
    let rows = vec![(
        "config/unresolved.properties".into(),
        None,
        b"pack".to_vec(),
        None,
    )];
    f.db.write_with(|tx| files.stage_pack_in(tx.connection(), &owner(), "unresolved-pack", &rows))
        .unwrap();
    std::fs::write(f.game.join("config/unresolved.properties"), b"user witness").unwrap();
    let probe_files = files.clone();
    let game = f.game.clone();
    let db = f.db.clone();
    *f.native.activation_action.lock().unwrap() = Some(Box::new(move || {
        assert!(
            db.read(|c| Ok(c.query_row(
                "SELECT recovery_required FROM software_state WHERE singleton=1",
                [],
                |r| r.get::<_, bool>(0)
            )?))
            .unwrap()
        );
        assert_eq!(
            crate::operations::recovery_start_probe(probe_files, &game),
            0
        );
    }));
    f.previous("previous launch");
    let p = h.prepare_rollback(&owner(), 0).unwrap();
    let done = completion(&h);
    h.rollback(&owner(), request(&p, "activation-pending-key"))
        .unwrap();
    done.recv_timeout(Duration::from_secs(10)).unwrap();
    assert_eq!(f.native.calls.load(Ordering::SeqCst), 1);
    drop(files);
    drop(h);
    drop(service);
    for _ in 0..3 {
        reload(&mut f);
        let files = crate::files::FileBrowser::open(f.db.clone(), &f.game, &[]).unwrap();
        assert_eq!(crate::operations::recovery_start_probe(files, &f.game), 0);
        assert!(f.db.directory().path().join("content-pending").exists());
        assert_eq!(
            std::fs::read(f.game.join("config/unresolved.properties")).unwrap(),
            b"user witness"
        );
        witnesses(&f);
    }
}
#[tokio::test]
async fn recovery_assurance_partial_pack_job_stays_failed_after_independent_file_recovery() {
    use crate::packs::workspace::*;
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    let mut f = owned();
    let world_witness = crate::world::tests::level(42, true);
    std::fs::write(f.game.join("world/level.dat"), &world_witness).unwrap();
    let service = f.open();
    let h = service.handle();
    f.db.write_with(|tx| -> crate::software::Result<()> {
        let c = tx.connection();
        let mut launch = store::state(c)?.current;
        launch.installation.software = Some(Software::Vanilla);
        launch.installation.version = Some("1.21.1".into());
        c.execute(
            "UPDATE software_state SET current_launch=? WHERE singleton=1",
            [store::encode(&launch)?],
        )?;
        Ok(())
    })
    .unwrap();
    let workspace = PackWorkspace::open(&f.db).unwrap();
    let releases = crate::packs::PackReleases::open(&f.db).unwrap();
    let files = crate::files::FileBrowser::open(f.db.clone(), &f.game, &[]).unwrap();
    let mut revision = "0".to_string();
    for n in 0..2 {
        let draft = workspace
            .file_mutate(
                &owner(),
                "1.21.1",
                &format!("fixture-file-{n}"),
                &FileMutation {
                    expected_revision: revision,
                    change: FileOperation::Write {
                        root: "overrides".into(),
                        path: format!("config/pack-{n}.properties"),
                        content_base64: STANDARD.encode(format!("pack-{n}")),
                    },
                },
            )
            .unwrap();
        revision = draft.revision;
    }
    let plan = workspace
        .install_plan(
            &owner(),
            "1.21.1",
            Some("vanilla"),
            &PackInstallRequest {
                analyze: false,
                source: InstallSource::Draft { revision },
            },
            &files,
            &releases,
            &crate::content::UreqProviderClient::new(None),
        )
        .unwrap();
    assert!(!plan.software_switch_required);
    let work = PackWork {
        workspace: workspace.clone(),
        files: files.clone(),
        key: "partial-job-key".into(),
        input: PackInstallApply {
            acknowledge_warnings: true,
            acknowledge_software_switch: false,
            plan_id: plan.plan_id,
            confirmation_token: plan.confirmation_token,
            choices: vec![],
            set_as_modpack: false,
        },
    };
    f.db.write(|c|{c.execute_batch("CREATE TRIGGER fail_pack_outer_commit BEFORE INSERT ON audit_events WHEN NEW.action='content.apply' BEGIN SELECT RAISE(ABORT,'owned-fixture'); END;")?;Ok(())}).unwrap();
    let done = completion(&h);
    let job = h.apply_pack(&owner(), work.clone()).await.unwrap();
    done.recv_timeout(Duration::from_secs(10)).unwrap();
    assert_eq!(h.jobs(&owner()).unwrap()[0].state, "failed");
    assert_eq!(f.native.calls.load(Ordering::SeqCst), 0);
    assert_eq!(
        crate::operations::recovery_start_probe(files.clone(), &f.game),
        0
    );
    assert_eq!(
        scalar(
            &f,
            "SELECT count(*) FROM audit_events WHERE action='files.write' AND outcome='success'"
        ),
        1
    );
    let replay = h.apply_pack(&owner(), work.clone()).await.unwrap();
    assert_eq!(replay.id, job.id);
    assert_eq!(replay.state, "failed");
    drop(work);
    drop(files);
    drop(workspace);
    drop(releases);
    drop(h);
    drop(service);
    f.db.write(|c| {
        c.execute_batch("DROP TRIGGER fail_pack_outer_commit")?;
        Ok(())
    })
    .unwrap();
    for _ in 0..3 {
        reload(&mut f);
        let service = f.open();
        let h = service.handle();
        let files = crate::files::FileBrowser::open(f.db.clone(), &f.game, &[]).unwrap();
        assert_eq!(crate::operations::recovery_start_probe(files, &f.game), 1);
        let jobs = h.jobs(&owner()).unwrap();
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].id, job.id);
        assert_eq!(
            jobs[0].state, "failed",
            "per-file roll-forward cannot rewrite interrupted job success"
        );
        assert_eq!(
            scalar(
                &f,
                "SELECT count(*) FROM server_content_changes WHERE state='pending'"
            ),
            0
        );
        assert_eq!(
            scalar(
                &f,
                "SELECT count(*) FROM audit_events WHERE action='files.write' AND outcome='success'"
            ),
            2
        );
        for n in 0..2 {
            assert_eq!(
                std::fs::read(f.game.join(format!("config/pack-{n}.properties"))).unwrap(),
                format!("pack-{n}").as_bytes()
            );
        }
        assert_eq!(
            std::fs::read(f.game.join("server.properties")).unwrap(),
            b"motd=preserved\n"
        );
        assert_eq!(
            std::fs::read(f.game.join("world/level.dat")).unwrap(),
            world_witness
        );
        assert_eq!(
            std::fs::read(f.game.join("config/managed-recovery.properties")).unwrap(),
            b"managed witness"
        );
        drop(h);
        drop(service);
    }
}
