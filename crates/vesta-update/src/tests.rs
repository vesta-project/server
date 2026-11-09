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
