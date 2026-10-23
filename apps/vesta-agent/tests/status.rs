//! Owned offline fixtures only; no listener, launcher, Java or game execution.
use std::{collections::BTreeMap, fs, path::Path, process::Command, sync::Arc};
use vesta_state::{StateDatabase, StateDirectory, SystemClock};

const DOCUMENT: &str = "schema-version = 1\n[paths]\nstate = 'private-state'\ngame = 'game'\nscan-roots = ['game/public']\nlaunch-definition = 'private/launch.toml'\n[pack]\nname = 'fixture-secret-display-name'\nversion = 'fixture-secret-version'\nrevision = 9\nminimum-revision = 5\nminecraft-version = '1.21.1'\n[pack-access]\nmode = 'minecraft'\n[listener]\nmode = 'disabled'\n";

fn fixture() -> tempfile::TempDir {
    let temp = tempfile::tempdir().unwrap();
    fs::create_dir_all(temp.path().join("game/public")).unwrap();
    fs::write(temp.path().join("host.toml"), DOCUMENT).unwrap();
    fs::write(temp.path().join("game/retained"), b"owned game bytes").unwrap();
    temp
}

fn database(root: &Path) -> StateDatabase {
    StateDatabase::open(
        StateDirectory::new(root.join("private-state"), Some(&root.join("game"))).unwrap(),
        Arc::new(SystemClock),
    )
    .unwrap()
}

fn command(root: &Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_vesta-agent"))
        .current_dir(root)
        .args(args)
        .output()
        .unwrap()
}

fn tree(root: &Path) -> BTreeMap<std::path::PathBuf, Vec<u8>> {
    fn visit(root: &Path, path: &Path, entries: &mut BTreeMap<std::path::PathBuf, Vec<u8>>) {
        for entry in fs::read_dir(path).unwrap() {
            let path = entry.unwrap().path();
            let relative = path.strip_prefix(root).unwrap().to_owned();
            if path.is_dir() {
                entries.insert(relative, Vec::new());
                visit(root, &path, entries);
            } else {
                entries.insert(relative, fs::read(&path).unwrap());
            }
        }
    }
    let mut entries = BTreeMap::new();
    visit(root, root, &mut entries);
    entries
}

#[test]
fn missing_state_refuses_without_creating_anything() {
    let temp = fixture();
    let before = tree(temp.path());
    let output = command(temp.path(), &["status", "host.toml"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert_eq!(output.stderr, b"status_failed: status_state_missing\n");
    assert_eq!(tree(temp.path()), before);
}

#[test]
fn empty_current_state_reports_configuration_and_zero_counts_without_writes() {
    let temp = fixture();
    let db = database(temp.path());
    db.close().unwrap();
    let before = tree(temp.path());
    let output = command(temp.path(), &["status", "host.toml"]);
    assert!(output.status.success(), "{:?}", output.stderr);
    assert!(output.stderr.is_empty());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "status_scope=quiescent_state; runtime_health=not_observed\nserver_id=[redacted]\nconfigured_pack_access=minecraft; configured_listener=disabled\nconfigured_pack_revision=9; configured_pack_minimum=5\nhosted_pack=none\nhosted_artifact=not_applicable; digest=not_verified\nfile_intents_pending=0; file_intents_recovery_required=0\noperations_pending=0; operations_recovery_required=0\nlast_audit_sequence=0\n"
    );
    assert_eq!(tree(temp.path()), before);
}

#[test]
fn hosted_release_counts_and_audit_sequence_are_redacted_and_observational() {
    let temp = fixture();
    let db = database(temp.path());
    let server = db.server_id().to_owned();
    db.write(|c| {
        c.execute("INSERT INTO pack_releases VALUES (?,3,2,'fixture-secret-version','zip',?,3,0)", rusqlite::params![server,"a".repeat(64)])?;
        c.execute("INSERT INTO pack_releases VALUES (?,1,1,'old','zip',?,3,0)", rusqlite::params![server,"b".repeat(64)])?;
        for (id,status) in [("c","pending"),("d","recovery-required"),("e","complete"),("f","aborted")] {
            c.execute("INSERT INTO file_intents VALUES (?,?,'secret-principal','secret-device','write','secret-target','{}',?,0)",rusqlite::params![id.repeat(32),server,status])?;
        }
        for (id,state) in [("pending","queued"),("running","running"),("recover","recovery_required"),("done","succeeded")] {
            c.execute("INSERT INTO operations (id,server_id,actor_principal_id,device_id,source,type,target,payload_hash,state,requires_confirmation,external_effect_idempotent,created_at,updated_at) VALUES (?,?,'secret-principal','secret-device','local','fixture','secret-target','secret-hash',?,0,0,0,0)",rusqlite::params![id,server,state])?;
        }
        c.execute("INSERT INTO audit_events (server_id,principal_id,device_id,source,action,target,outcome,at,metadata_json,prev_hash,hash) VALUES (?,'secret-principal','secret-device','local','fixture','secret-target','success',0,'{\"secret\":true}','secret-prev','secret-hash')",[&server])?;
        Ok(())
    }).unwrap();
    fs::create_dir(temp.path().join("private-state/artifacts")).unwrap();
    let artifact = temp
        .path()
        .join(format!("private-state/artifacts/{}.zip", "a".repeat(64)));
    fs::write(&artifact, b"zip").unwrap();
    db.close().unwrap();
    for present in [true, false] {
        if !present {
            fs::remove_file(&artifact).unwrap();
        }
        let before = tree(temp.path());
        let output = command(temp.path(), &["status", "host.toml"]);
        assert!(output.status.success(), "{:?}", output.stderr);
        assert!(output.stderr.is_empty());
        let text = String::from_utf8(output.stdout).unwrap();
        assert!(text.contains("hosted_pack=revision=3; minimum=2"));
        assert!(text.contains(if present {
            "hosted_artifact=present"
        } else {
            "hosted_artifact=missing_or_size_mismatch"
        }));
        assert!(text.contains("file_intents_pending=1; file_intents_recovery_required=1"));
        assert!(text.contains("operations_pending=2; operations_recovery_required=1"));
        assert!(text.contains("last_audit_sequence=1"));
        for sensitive in [
            server.as_str(),
            "secret",
            temp.path().to_str().unwrap(),
            &"a".repeat(64),
        ] {
            assert!(!text.contains(sensitive));
        }
        assert_eq!(tree(temp.path()), before);
    }
}

#[test]
fn companions_refuse_without_recovery_or_new_files() {
    let temp = fixture();
    database(temp.path()).close().unwrap();
    for suffix in ["-wal", "-shm", "-journal"] {
        let path = temp
            .path()
            .join(format!("private-state/vesta-state.db{suffix}"));
        fs::write(&path, b"fixture-secret-journal").unwrap();
        let before = tree(temp.path());
        let output = command(temp.path(), &["status", "host.toml"]);
        assert_eq!(output.status.code(), Some(1));
        assert!(output.stdout.is_empty());
        assert_eq!(
            output.stderr,
            b"status_failed: status_requires_quiescent_state\n"
        );
        assert_eq!(tree(temp.path()), before);
        fs::remove_file(path).unwrap();
    }
}

#[test]
fn corrupt_and_old_schema_state_refuse_with_fixed_errors_without_migration() {
    for corrupt in [true, false] {
        let temp = fixture();
        database(temp.path()).close().unwrap();
        let path = temp.path().join("private-state/vesta-state.db");
        if corrupt {
            fs::write(&path, b"fixture-secret-not-sqlite").unwrap();
        } else {
            let connection = rusqlite::Connection::open(&path).unwrap();
            connection
                .execute_batch("PRAGMA journal_mode=DELETE; PRAGMA user_version=1;")
                .unwrap();
        }
        let before = tree(temp.path());
        let output = command(temp.path(), &["status", "host.toml"]);
        assert_eq!(output.status.code(), Some(1));
        assert!(output.stdout.is_empty());
        assert_eq!(output.stderr, b"status_failed: status_state_unavailable\n");
        assert_eq!(tree(temp.path()), before);
    }
}

#[test]
fn invalid_configuration_is_redacted_and_does_not_touch_state() {
    let temp = fixture();
    fs::write(temp.path().join("host.toml"), b"fixture-secret = true").unwrap();
    let before = tree(temp.path());
    let output = command(temp.path(), &["status", "host.toml"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert_eq!(output.stderr, b"status_failed: status_config_unavailable\n");
    assert_eq!(tree(temp.path()), before);
}

#[test]
fn strict_arguments_refuse_unknown_flags_and_design_only_mutations() {
    let temp = fixture();
    let before = tree(temp.path());
    for args in [
        vec!["status"],
        vec!["status", "--help"],
        vec!["status", "--config", "host.toml"],
        vec!["status", "host.toml", "extra"],
        vec!["--help", "extra"],
        vec!["publish", "host.toml"],
        vec!["install", "host.toml"],
        vec!["adopt", "host.toml"],
        vec!["owner-setup", "host.toml"],
        vec!["audit-tail", "host.toml"],
    ] {
        let output = command(temp.path(), &args);
        assert_eq!(output.status.code(), Some(2), "{args:?}");
        assert!(output.stdout.is_empty());
        assert!(output.stderr.starts_with(b"Vesta agent\n"));
        assert_eq!(tree(temp.path()), before);
    }
}

#[test]
fn alternate_pack_access_mode_is_configuration_not_administration() {
    let temp = fixture();
    fs::write(
        temp.path().join("host.toml"),
        DOCUMENT.replace("mode = 'minecraft'", "mode = 'minecraft-or-password'"),
    )
    .unwrap();
    database(temp.path()).close().unwrap();
    let before = tree(temp.path());
    let output = command(temp.path(), &["status", "host.toml"]);
    assert!(output.status.success(), "{:?}", output.stderr);
    assert!(
        String::from_utf8(output.stdout)
            .unwrap()
            .contains("configured_pack_access=minecraft-or-password")
    );
    assert_eq!(tree(temp.path()), before);
}

#[test]
fn open_wal_database_refuses_without_touching_the_running_hosts_state() {
    let temp = fixture();
    let db = database(temp.path());
    assert!(
        temp.path()
            .join("private-state/vesta-state.db-wal")
            .exists()
    );
    let before = tree(temp.path());
    let output = command(temp.path(), &["status", "host.toml"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert_eq!(
        output.stderr,
        b"status_failed: status_requires_quiescent_state\n"
    );
    assert_eq!(tree(temp.path()), before);
    db.close().unwrap();
}

#[test]
fn artifact_size_mismatch_is_reported_without_modifying_bytes() {
    let temp = fixture();
    let db = database(temp.path());
    db.write(|c| {
        c.execute(
            "INSERT INTO pack_releases VALUES (?,3,2,'fixture','zip',?,10,0)",
            rusqlite::params![db.server_id(), "a".repeat(64)],
        )?;
        Ok(())
    })
    .unwrap();
    fs::create_dir(temp.path().join("private-state/artifacts")).unwrap();
    fs::write(
        temp.path()
            .join(format!("private-state/artifacts/{}.zip", "a".repeat(64))),
        b"short",
    )
    .unwrap();
    db.close().unwrap();
    let before = tree(temp.path());
    let output = command(temp.path(), &["status", "host.toml"]);
    assert!(output.status.success(), "{:?}", output.stderr);
    assert!(
        String::from_utf8(output.stdout)
            .unwrap()
            .contains("hosted_artifact=missing_or_size_mismatch")
    );
    assert_eq!(tree(temp.path()), before);
}
