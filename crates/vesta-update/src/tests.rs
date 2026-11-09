use super::*;
use base64::Engine as _;
use ring::{
    rand::SystemRandom,
    signature::{Ed25519KeyPair, KeyPair},
};
use sha2::{Digest, Sha256};
use std::{fs, path::PathBuf};
const NOW: i64 = 1_800_000_000_000;

#[test]
fn startup_checks_are_opt_in_metadata_only_and_deduplicated_after_reopen() {
    let f=Fixture::new();let store=f.initialized();let trust=store.trust().unwrap();
    assert!(store.check_on_start(&"0".repeat(64),NOW).unwrap().is_none());
    assert!(store.last_startup_check().unwrap().is_none());
    let (source,m)=f.bundle(2);
    // No executable/manifest data is needed for discovery.
    fs::remove_file(source.local.as_ref().unwrap().join("manifest.json")).unwrap();
    store.configure_startup_check(&StartupPolicy{check_on_start:true,minimum_interval_ms:60000,source}).unwrap();
    let result=store.check_on_start(&"0".repeat(64),NOW).unwrap().unwrap();
    assert_eq!(result.outcome,CheckOutcome::Available);assert_eq!(result.candidate.unwrap(),m);
    assert!(store.status().unwrap().is_empty());assert_eq!(store.trust().unwrap(),trust);
    drop(store);let store=UpdateStore::open(&f.root()).unwrap();
    assert_eq!(store.last_startup_check().unwrap().unwrap().outcome,CheckOutcome::Available);
    assert!(store.check_on_start(&m.manifest_sha256,NOW+59999).unwrap().is_none());
    assert_eq!(store.check_on_start(&m.manifest_sha256,NOW+60000).unwrap().unwrap().outcome,CheckOutcome::Current);
    assert_eq!(store.trust().unwrap(),trust);
}

#[test]
fn startup_failures_and_interrupted_attempts_preserve_trust_and_space_retries() {
    let f=Fixture::new();let store=f.initialized();let trust=store.trust().unwrap();let (source,_)=f.bundle(2);
    store.configure_startup_check(&StartupPolicy{check_on_start:true,minimum_interval_ms:1000,source:source.clone()}).unwrap();
    fs::write(source.local.as_ref().unwrap().join("update.sig"),b"fake-secret-do-not-propagate").unwrap();
    let failed=store.check_on_start(&"0".repeat(64),NOW).unwrap().unwrap();
    assert_eq!(failed.outcome,CheckOutcome::Failed);assert_eq!(failed.error_code.as_deref(),Some("update_signature_invalid"));
    assert!(!serde_json::to_string(&failed).unwrap().contains("fake-secret"));
    assert_eq!(store.trust().unwrap(),trust);assert!(store.status().unwrap().is_empty());
    // Emulate durable admission followed by process interruption, without killing anything.
    let incomplete=serde_json::to_string(&StartupCheck{attempted_at_ms:NOW+1000,outcome:CheckOutcome::Incomplete,candidate:None,error_code:None}).unwrap();
    store.db.write(|c| {c.execute("UPDATE update_startup_check SET document=?",[incomplete])?;Ok(())}).unwrap();
    drop(store);let store=UpdateStore::open(&f.root()).unwrap();
    assert!(store.check_on_start(&"0".repeat(64),NOW+1999).unwrap().is_none());
    assert!(store.check_on_start(&"0".repeat(64),NOW-1).unwrap().is_none());
    assert_eq!(store.last_startup_check().unwrap().unwrap().outcome,CheckOutcome::Incomplete);
    assert_eq!(store.check_on_start(&"0".repeat(64),NOW+2000).unwrap().unwrap().outcome,CheckOutcome::Failed);
}

#[test]
fn startup_configuration_is_local_explicit_and_never_replaces_trust() {
    let f=Fixture::new();let store=f.initialized();let trust=store.trust().unwrap();let (source,_)=f.bundle(2);
    let mut policy=StartupPolicy{check_on_start:true,minimum_interval_ms:1,source};
    let uninitialized=UpdateStore::open(&f.temp.path().join("no-trust")).unwrap();
    assert_eq!(uninitialized.configure_startup_check(&policy).unwrap_err(),"update_trust_not_initialized");
    for interval in [0,u64::MAX] {policy.minimum_interval_ms=interval;assert!(store.configure_startup_check(&policy).is_err());}
    policy.minimum_interval_ms=1;
    for url in ["http://example.test/releases","https://user:password@example.test/releases","https://example.test/releases?secret=value"] {
        policy.source=UpdateSource{local:None,base_url:Some(url.into()),asset_prefix:None};
        assert_eq!(store.configure_startup_check(&policy).unwrap_err(),"update_startup_source_invalid");
    }
    policy.source=UpdateSource{local:Some("relative-dir".into()),base_url:None,asset_prefix:None};
    assert!(store.configure_startup_check(&policy).is_err());
    policy.source=UpdateSource{local:None,base_url:Some("https://example.test/releases".into()),asset_prefix:Some("wrong-target-".into())};
    assert!(store.configure_startup_check(&policy).is_err());
    policy.source.asset_prefix=Some(format!("{}-",target().unwrap()));
    policy.check_on_start=false;store.configure_startup_check(&policy).unwrap();
    assert!(store.check_on_start(&"0".repeat(64),NOW).unwrap().is_none());
    assert_eq!(store.trust().unwrap(),trust);assert!(store.last_startup_check().unwrap().is_none());
    assert!(store.check_on_start("../escape",NOW).is_err());
    assert!(store.check_on_start(&"0".repeat(64),-1).is_err());
}

#[test]
fn startup_check_shares_update_lock_and_reconfiguration_clears_only_observation() {
    let f=Fixture::new();let store=f.initialized();let trust=store.trust().unwrap();let (source,_)=f.bundle(2);
    let policy=StartupPolicy{check_on_start:true,minimum_interval_ms:60000,source};
    store.configure_startup_check(&policy).unwrap();
    let lock=store.lock().unwrap();
    assert_eq!(store.check_on_start(&"0".repeat(64),NOW).unwrap_err(),"update_busy");
    assert_eq!(store.configure_startup_check(&policy).unwrap_err(),"update_busy");
    assert!(store.last_startup_check().unwrap().is_none());drop(lock);
    store.check_on_start(&"0".repeat(64),NOW).unwrap().unwrap();
    store.configure_startup_check(&policy).unwrap();assert!(store.last_startup_check().unwrap().is_none());
    assert!(store.check_on_start(&"0".repeat(64),NOW).unwrap().is_some());assert_eq!(store.trust().unwrap(),trust);
}

#[test]
fn startup_removed_source_and_expired_metadata_remain_failed_observations() {
    let f=Fixture::new();let store=f.initialized();let (source,m)=f.bundle(2);
    let policy=StartupPolicy{check_on_start:true,minimum_interval_ms:1,source:source.clone()};
    store.configure_startup_check(&policy).unwrap();
    let expired=store.check_on_start(&m.manifest_sha256,m.expires_at_ms).unwrap().unwrap();
    assert_eq!(expired.outcome,CheckOutcome::Failed);assert_eq!(expired.error_code.as_deref(),Some("update_metadata_expired"));
    fs::rename(source.local.as_ref().unwrap(),f.temp.path().join("offline-bundle")).unwrap();
    let missing=store.check_on_start(&m.manifest_sha256,m.expires_at_ms+1).unwrap().unwrap();
    assert_eq!(missing.outcome,CheckOutcome::Failed);assert_eq!(missing.error_code.as_deref(),Some("update_startup_source_invalid"));
    let mut disabled=policy;disabled.check_on_start=false;
    store.configure_startup_check(&disabled).unwrap();
    assert!(store.check_on_start(&m.manifest_sha256,m.expires_at_ms+2).unwrap().is_none());
}

#[test]
fn staged_observation_reverifies_saved_signatures_offline_after_reopen() {
    let f=Fixture::new();let store=f.initialized();let (source,m)=f.bundle(2);
    let staged=store.stage(&source,NOW,&mut|_,_,_|{}).unwrap();
    // The original download source is no longer authoritative or available.
    fs::write(source.local.as_ref().unwrap().join("manifest.json"),b"unavailable").unwrap();
    drop(store);let store=UpdateStore::open(&f.root()).unwrap();
    let trust=store.trust().unwrap();let checked=store.verify_staged(&m.manifest_sha256,NOW+1).unwrap();
    assert_eq!(checked.files,4);assert!(checked.bytes>0);assert!(!checked.activation_supported);
    assert_eq!(checked.version,m.version);assert_eq!(checked.manifest_sha256,staged.manifest_sha256);
    assert_eq!(store.trust().unwrap(),trust);
}

#[test]
fn staged_flag_does_not_hide_same_size_binary_tampering() {
    let f=Fixture::new();let store=f.initialized();let (source,m)=f.bundle(2);
    let staged=store.stage(&source,NOW,&mut|_,_,_|{}).unwrap();
    let name=if cfg!(windows){"vesta.exe"}else{"vesta"};let p=staged.directory.join("bin").join(name);
    let mut bytes=fs::read(&p).unwrap();bytes[0]^=1;fs::write(&p,&bytes).unwrap();
    assert_eq!(store.status().unwrap()[0].phase,"staged");
    assert_eq!(store.verify_staged(&m.manifest_sha256,NOW).unwrap_err(),"update_candidate_hash_mismatch");
}

#[test]
fn staged_observation_refuses_missing_or_hard_linked_binary_without_recreation() {
    let f=Fixture::new();let store=f.initialized();let (source,m)=f.bundle(2);
    let staged=store.stage(&source,NOW,&mut|_,_,_|{}).unwrap();
    let bin=staged.directory.join("bin");let hidden=staged.directory.join("hidden-bin");
    fs::rename(&bin,&hidden).unwrap();
    assert_eq!(store.verify_staged(&m.manifest_sha256,NOW).unwrap_err(),"update_candidate_unavailable");
    assert!(!bin.exists());fs::rename(hidden,&bin).unwrap();
    let name=if cfg!(windows){"vesta.exe"}else{"vesta"};let p=bin.join(name);
    let other=f.temp.path().join("hard-link");fs::hard_link(&p,&other).unwrap();
    assert_eq!(store.verify_staged(&m.manifest_sha256,NOW).unwrap_err(),"update_candidate_unavailable");
    fs::remove_file(other).unwrap();fs::remove_file(p).unwrap();
    assert_eq!(store.verify_staged(&m.manifest_sha256,NOW).unwrap_err(),"update_candidate_file_missing");
}

#[test]
fn staged_observation_rejects_expired_or_superseded_release_and_changed_proof() {
    let f=Fixture::new();let store=f.initialized();let (source,m)=f.bundle(2);
    store.stage(&source,NOW,&mut|_,_,_|{}).unwrap();
    assert_eq!(store.verify_staged(&m.manifest_sha256,m.expires_at_ms).unwrap_err(),"update_metadata_expired");
    let (newer,latest)=f.bundle(3);store.stage(&newer,NOW,&mut|_,_,_|{}).unwrap();
    assert_eq!(store.verify_staged(&m.manifest_sha256,NOW).unwrap_err(),"update_rollback_refused");
    store.db.write(|c| {c.execute("UPDATE update_manifest_proofs SET document=? WHERE hash=?",rusqlite::params![b"{}".as_slice(),latest.manifest_sha256])?;Ok(())}).unwrap();
    assert_eq!(store.verify_staged(&latest.manifest_sha256,NOW).unwrap_err(),"manifest hash mismatch");
}

#[test]
fn staged_observation_does_not_adopt_legacy_missing_proof_or_incomplete_stage() {
    let f=Fixture::new();let store=f.initialized();let (source,m)=f.bundle(2);
    store.stage(&source,NOW,&mut|_,_,_|{}).unwrap();
    store.db.write(|c| {c.execute("DELETE FROM update_manifest_proofs",[])?;Ok(())}).unwrap();
    assert_eq!(store.verify_staged(&m.manifest_sha256,NOW).unwrap_err(),"update_candidate_proof_unavailable");
    store.db.write(|c| {c.execute("UPDATE update_stages SET phase='staging'",[])?;Ok(())}).unwrap();
    assert_eq!(store.verify_staged(&m.manifest_sha256,NOW).unwrap_err(),"update_candidate_not_staged");
    assert_eq!(store.verify_staged("../escape",NOW).unwrap_err(),"update_candidate_invalid");
}

#[cfg(unix)]
#[test]
fn staged_observation_refuses_binary_or_parent_symlink_even_with_correct_bytes() {
    use std::os::unix::fs::symlink;
    let f=Fixture::new();let store=f.initialized();let (source,m)=f.bundle(2);
    let staged=store.stage(&source,NOW,&mut|_,_,_|{}).unwrap();let bin=staged.directory.join("bin");
    let p=bin.join("vesta");let saved=f.temp.path().join("correct-vesta");
    fs::rename(&p,&saved).unwrap();symlink(&saved,&p).unwrap();
    assert_eq!(store.verify_staged(&m.manifest_sha256,NOW).unwrap_err(),"update_candidate_unavailable");
    fs::remove_file(&p).unwrap();fs::rename(saved,&p).unwrap();
    let saved_bin=f.temp.path().join("correct-bin");fs::rename(&bin,&saved_bin).unwrap();symlink(&saved_bin,&bin).unwrap();
    assert_eq!(store.verify_staged(&m.manifest_sha256,NOW).unwrap_err(),"update_candidate_unavailable");
}
struct Fixture {
    temp: tempfile::TempDir,
    key: Ed25519KeyPair,
}
impl Fixture {
    fn new() -> Self {
        Self {
            temp: tempfile::tempdir().unwrap(),
            key: Ed25519KeyPair::from_pkcs8(
                Ed25519KeyPair::generate_pkcs8(&SystemRandom::new())
                    .unwrap()
                    .as_ref(),
            )
            .unwrap(),
        }
    }
    fn pin(&self) -> String {
        hex::encode(self.key.public_key().as_ref())
    }
    fn root(&self) -> PathBuf {
        self.temp.path().join("private-update-state")
    }
    fn bundle(&self, sequence: u64) -> (UpdateSource, Metadata) {
        let root = self.temp.path().join(format!("bundle-{sequence}"));
        fs::create_dir(&root).unwrap();
        let suffix = if cfg!(windows) { ".exe" } else { "" };
        let files: Vec<_> = ["vesta-setup", "vesta-agent", "vesta-supervisor", "vesta"]
            .iter()
            .map(|name| {
                let name = format!("{name}{suffix}");
                let bytes = format!("inert fixture {sequence} {name}").into_bytes();
                fs::write(root.join(&name), &bytes).unwrap();
                release::Artifact {
                    name,
                    size: bytes.len() as u64,
                    sha256: hex::encode(Sha256::digest(&bytes)),
                }
            })
            .collect();
        let manifest = release::Manifest {
            schema: 1,
            version: format!("nightly-{sequence}"),
            target: target().unwrap().into(),
            files,
        };
        let bytes = serde_json::to_vec(&manifest).unwrap();
        fs::write(root.join("manifest.json"), &bytes).unwrap();
        fs::write(
            root.join("manifest.sig"),
            base64::engine::general_purpose::STANDARD.encode(self.key.sign(&bytes).as_ref()),
        )
        .unwrap();
        let metadata = Metadata {
            schema: 1,
            purpose: "vesta-update-release-v1".into(),
            channel: "nightly".into(),
            sequence,
            version: manifest.version,
            target: manifest.target,
            manifest_sha256: hex::encode(Sha256::digest(&bytes)),
            issued_at_ms: NOW - 1000,
            expires_at_ms: NOW + 86_400_000,
        };
        let source = UpdateSource {
            local: Some(root),
            base_url: None,
            asset_prefix: None,
        };
        self.sign(&source, &metadata);
        (source, metadata)
    }
    fn sign(&self, source: &UpdateSource, m: &Metadata) {
        let bytes = serde_json::to_vec(m).unwrap();
        let root = source.local.as_ref().unwrap();
        fs::write(root.join("update.json"), &bytes).unwrap();
        fs::write(
            root.join("update.sig"),
            base64::engine::general_purpose::STANDARD.encode(self.key.sign(&bytes).as_ref()),
        )
        .unwrap();
    }
    fn initialized(&self) -> UpdateStore {
        let (source, m) = self.bundle(1);
        let store = UpdateStore::open(&self.root()).unwrap();
        store
            .initialize(&self.pin(), "nightly", &m.manifest_sha256, &source, NOW)
            .unwrap();
        store
    }
}
#[test]
fn bootstrap_requires_installed_release_pin_and_never_accepts_a_new_key() {
    let f = Fixture::new();
    let (source, m) = f.bundle(1);
    let store = UpdateStore::open(&f.root()).unwrap();
    assert!(
        store
            .initialize(&f.pin(), "nightly", &"0".repeat(64), &source, NOW)
            .is_err()
    );
    assert!(store.trust().is_err());
    let trust = store
        .initialize(&f.pin(), "nightly", &m.manifest_sha256, &source, NOW)
        .unwrap();
    assert_eq!(trust.sequence, 1);
    assert_eq!(trust.release_key, f.pin());
    assert_eq!(trust.fingerprint().len(), 64);
    assert!(
        store
            .initialize(
                &Fixture::new().pin(),
                "nightly",
                &m.manifest_sha256,
                &source,
                NOW
            )
            .is_err()
    );
    assert!(
        store
            .initialize(&f.pin(), "stable", &m.manifest_sha256, &source, NOW)
            .is_err()
    );
}
#[test]
fn signed_metadata_rejects_tampering_wrong_context_time_and_replay() {
    let f = Fixture::new();
    let store = f.initialized();
    let (source, m) = f.bundle(2);
    assert_eq!(store.check(&source, NOW).unwrap(), m);
    assert_eq!(
        store.trust().unwrap().sequence,
        1,
        "check has no sequence effect"
    );
    for mutate in [0, 1, 2, 3, 4, 5] {
        let mut changed = m.clone();
        match mutate {
            0 => changed.channel = "stable".into(),
            1 => changed.target = "wrong-target".into(),
            2 => changed.purpose = "other-protocol".into(),
            3 => changed.expires_at_ms = NOW,
            4 => changed.issued_at_ms = NOW + 300001,
            _ => changed.sequence = 0,
        };
        f.sign(&source, &changed);
        assert!(store.check(&source, NOW).is_err());
    }
    f.sign(&source, &m);
    let root = source.local.as_ref().unwrap();
    let mut bytes = fs::read(root.join("update.json")).unwrap();
    bytes.push(b' ');
    fs::write(root.join("update.json"), bytes).unwrap();
    assert!(store.check(&source, NOW).is_err());
    f.sign(&source, &m);
    store.stage(&source, NOW + 1, &mut |_, _, _| {}).unwrap();
    let old = UpdateSource {
        local: Some(f.temp.path().join("bundle-1")),
        base_url: None,
        asset_prefix: None,
    };
    assert_eq!(
        store.check(&old, NOW + 1).unwrap_err(),
        "update_rollback_refused"
    );
    assert_eq!(
        store.check(&source, NOW).unwrap_err(),
        "update_clock_rollback"
    );
    let mut changed = m.clone();
    changed.version = "substitution".into();
    f.sign(&source, &changed);
    assert_eq!(
        store.check(&source, NOW + 2).unwrap_err(),
        "update_rollback_refused"
    );
}
#[test]
fn staging_is_resumable_reverified_and_never_replaces_active_binaries() {
    let f = Fixture::new();
    let store = f.initialized();
    let active = f.temp.path().join("active-agent");
    fs::write(&active, b"keep active").unwrap();
    let (source, m) = f.bundle(2);
    let first = &release::load(release::Source {
        local: source.local.clone(),
        base_url: None,
        asset_prefix: None,
        manifest_sha256: m.manifest_sha256.clone(),
        release_key: f.pin(),
    })
    .unwrap()
    .manifest
    .files[1]
        .name;
    let original = fs::read(source.local.as_ref().unwrap().join(first)).unwrap();
    fs::write(source.local.as_ref().unwrap().join(first), b"tampered").unwrap();
    assert!(store.stage(&source, NOW, &mut |_, _, _| {}).is_err());
    assert_eq!(store.status().unwrap()[0].phase, "staging");
    assert_eq!(store.trust().unwrap().sequence, 2);
    fs::write(source.local.as_ref().unwrap().join(first), &original).unwrap();
    let staged = store.stage(&source, NOW, &mut |_, _, _| {}).unwrap();
    assert_eq!(staged.phase, "staged");
    assert_eq!(fs::read(&active).unwrap(), b"keep active");
    drop(store);
    let reopened = UpdateStore::open(&f.root()).unwrap();
    assert_eq!(reopened.status().unwrap()[0].phase, "staged");
    reopened.stage(&source, NOW, &mut |_, _, _| {}).unwrap();
    fs::write(staged.directory.join("bin").join(first), b"local tamper").unwrap();
    assert!(reopened.stage(&source, NOW, &mut |_, _, _| {}).is_err());
}
#[test]
fn staging_has_an_exclusive_cross_handle_lock() {
    let f = Fixture::new();
    let store = f.initialized();
    let (source, _) = f.bundle(2);
    let other = UpdateStore::open(&f.root()).unwrap();
    let (entered, seen) = std::sync::mpsc::channel();
    let (release, resume) = std::sync::mpsc::channel();
    let remote = source.clone();
    let worker = std::thread::spawn(move || {
        let mut first = true;
        store.stage(&remote, NOW, &mut |_, _, _| {
            if first {
                first = false;
                entered.send(()).unwrap();
                resume.recv().unwrap();
            }
        })
    });
    seen.recv_timeout(std::time::Duration::from_secs(5))
        .unwrap();
    assert_eq!(
        other.stage(&source, NOW, &mut |_, _, _| {}).unwrap_err(),
        "update_busy"
    );
    release.send(()).unwrap();
    worker.join().unwrap().unwrap();
}
