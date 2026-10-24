use super::*;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use ring::{
    rand::SystemRandom,
    signature::{Ed25519KeyPair, KeyPair},
};
use russh::{
    client,
    keys::{PrivateKeyWithHashAlg, PublicKeyOrCertificate},
};
use russh_sftp::{
    client::RawSftpSession,
    protocol::{FileAttributes, OpenFlags},
};
use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicI64, Ordering},
    time::Duration,
};
use vesta_state::{Clock, StateDirectory};
struct TestClock(AtomicI64);
impl Clock for TestClock {
    fn millis(&self) -> i64 {
        self.0.load(Ordering::SeqCst)
    }
}
struct Fixture {
    temp: tempfile::TempDir,
    game: PathBuf,
    service: Arc<SftpService>,
    token: String,
    principal: String,
    key: Arc<PrivateKey>,
    key_id: String,
    clock: Arc<TestClock>,
}
impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let game = temp.path().join("game");
        fs::create_dir(&game).unwrap();
        let clock = Arc::new(TestClock(AtomicI64::new(1791115200000)));
        let db = StateDatabase::open(
            StateDirectory::new(temp.path().join("state"), Some(&game)).unwrap(),
            clock.clone(),
        )
        .unwrap();
        let management = ManagementAccess::new(db.clone()).unwrap();
        let service = SftpService::open(
            db.clone(),
            FileBrowser::open(db, &game, &[]).unwrap(),
            management,
        )
        .unwrap();
        let management_key = Ed25519KeyPair::from_pkcs8(
            Ed25519KeyPair::generate_pkcs8(&SystemRandom::new())
                .unwrap()
                .as_ref(),
        )
        .unwrap();
        let code = service
            .management
            .issue_local(BuiltinRole::Owner, 10)
            .unwrap();
        let enrolled = service
            .management
            .enroll(
                code.secret.expose(),
                "SFTP fixture",
                &URL_SAFE_NO_PAD.encode(management_key.public_key().as_ref()),
            )
            .unwrap();
        let nonce = service.management.challenge(&enrolled.device_id).unwrap();
        let signature = management_key.sign(&identity::management::session_message(
            service.db.server_id(),
            nonce.expose(),
        ));
        let token = service
            .management
            .session(
                &enrolled.device_id,
                nonce.expose(),
                &URL_SAFE_NO_PAD.encode(signature.as_ref()),
            )
            .unwrap()
            .secret
            .expose()
            .to_owned();
        let key =
            Arc::new(PrivateKey::random(&mut rand::rng(), ssh_key::Algorithm::Ed25519).unwrap());
        let row = service
            .register_key(
                &token,
                KeyRegistration {
                    principal_id: enrolled.principal_id.clone(),
                    label: "SSH fixture".into(),
                    public_key: key.public_key().to_openssh().unwrap(),
                    expires_at_ms: None,
                },
            )
            .unwrap();
        Self {
            temp,
            game,
            service,
            token,
            principal: enrolled.principal_id,
            key,
            key_id: row["id"].as_str().unwrap().into(),
            clock,
        }
    }
    async fn enable(&self) -> SocketAddr {
        let reserved = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = reserved.local_addr().unwrap().port();
        drop(reserved);
        let status = self
            .service
            .patch(
                &self.token,
                serde_json::from_value(json!({"enabled":true,"port":port})).unwrap(),
            )
            .await
            .unwrap();
        status["bound_address"].as_str().unwrap().parse().unwrap()
    }
    async fn client(&self, address: SocketAddr) -> client::Handle<Client> {
        let key = self.service.host_key().unwrap();
        let mut client = client::connect(
            Arc::new(client::Config::default()),
            address,
            Client(
                key.public_key()
                    .fingerprint(ssh_key::HashAlg::Sha256)
                    .to_string(),
            ),
        )
        .await
        .unwrap();
        assert!(
            client
                .authenticate_publickey(
                    &self.principal,
                    PrivateKeyWithHashAlg::new(self.key.clone(), None)
                )
                .await
                .unwrap()
                .success()
        );
        client
    }
    async fn sftp(&self, address: SocketAddr) -> (client::Handle<Client>, RawSftpSession) {
        let client = self.client(address).await;
        let channel = client.channel_open_session().await.unwrap();
        channel.request_subsystem(true, "sftp").await.unwrap();
        let sftp = RawSftpSession::new(channel.into_stream());
        sftp.set_timeout(3);
        sftp.init().await.unwrap();
        (client, sftp)
    }
}
struct Client(String);
#[test]
fn management_receipts_replay_and_recheck_authority() {
    let f = Fixture::new();
    let cap = Capability::CredentialsManage;
    assert!(
        f.service
            .begin_mutation(&f.token, cap, "key-once", "sftp/keys", b"input")
            .unwrap()
            .is_none()
    );
    assert_eq!(
        f.service
            .begin_mutation(&f.token, cap, "key-once", "sftp/keys", b"input")
            .unwrap_err()
            .code(),
        "sftp_outcome_unknown"
    );
    let expected = json!({"id":"one-device"});
    let result = Ok(expected.clone());
    f.service.finish_mutation("key-once", &result).unwrap();
    assert_eq!(
        f.service
            .begin_mutation(&f.token, cap, "key-once", "sftp/keys", b"input")
            .unwrap()
            .unwrap()
            .unwrap(),
        expected
    );
    assert_eq!(
        f.service
            .begin_mutation(&f.token, cap, "key-once", "sftp/keys", b"changed")
            .unwrap_err()
            .code(),
        "sftp_idempotency_conflict"
    );
    f.service
        .db
        .write(|c| {
            c.execute(
                "UPDATE role_assignments SET revoked_at=? WHERE principal_id=?",
                params![f.clock.millis(), f.principal],
            )?;
            Ok(())
        })
        .unwrap();
    assert!(
        f.service
            .begin_mutation(&f.token, cap, "key-once", "sftp/keys", b"input")
            .is_err()
    );
}
impl client::Handler for Client {
    type Error = russh::Error;
    async fn check_server_key(
        &mut self,
        key: &PublicKeyOrCertificate,
    ) -> std::result::Result<bool, Self::Error> {
        Ok(key
            .public_key()
            .fingerprint(ssh_key::HashAlg::Sha256)
            .to_string()
            == self.0)
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn disabled_default_has_no_key_or_listener_and_management_refuses_invalid_token() {
    let f = Fixture::new();
    f.service.start().await.unwrap();
    let s = f.service.status(&f.token).unwrap();
    assert_eq!(s["running"], false);
    assert!(s["host_key_fingerprint"].is_null());
    assert!(f.service.status("bogus").is_err());
    assert!(f.service.keys("bogus").is_err());
    assert!(serde_json::from_value::<ConfigPatch>(json!({"password_enabled":true})).is_err());
    let first = f
        .service
        .host_key()
        .unwrap()
        .public_key()
        .fingerprint(ssh_key::HashAlg::Sha256);
    let reopened = SftpService::open(
        f.service.db.clone(),
        f.service.files.clone(),
        f.service.management.clone(),
    )
    .unwrap();
    assert_eq!(
        reopened
            .host_key()
            .unwrap()
            .public_key()
            .fingerprint(ssh_key::HashAlg::Sha256),
        first
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn real_ssh_sftp_live_upload_trash_rename_read_and_revoke() {
    let f = Fixture::new();
    fs::write(f.game.join("file.txt"), b"original").unwrap();
    let address = f.enable().await;
    let (_client, sftp) = f.sftp(address).await;
    assert_eq!(sftp.realpath(".").await.unwrap().files[0].filename, "/");
    let handle = sftp
        .open(
            "/file.txt",
            OpenFlags::WRITE | OpenFlags::TRUNCATE,
            FileAttributes::default(),
        )
        .await
        .unwrap()
        .handle;
    assert_eq!(fs::read(f.game.join("file.txt")).unwrap(), b"");
    sftp.write(&handle, 0, b"partial".to_vec()).await.unwrap();
    assert_eq!(fs::read(f.game.join("file.txt")).unwrap(), b"partial");
    sftp.write(&handle, 7, b" complete".to_vec()).await.unwrap();
    sftp.close(&handle).await.unwrap();
    sftp.rename("file.txt", "renamed.txt").await.unwrap();
    let read = sftp
        .open("renamed.txt", OpenFlags::READ, FileAttributes::default())
        .await
        .unwrap()
        .handle;
    assert_eq!(
        sftp.read(&read, 0, 100).await.unwrap().data,
        b"partial complete"
    );
    for p in [
        "../outside",
        "C:/Windows/win.ini",
        "//host/share/file",
        "/server.properties",
        "/.vesta/secret",
    ] {
        assert!(
            sftp.open(p, OpenFlags::READ, FileAttributes::default())
                .await
                .is_err(),
            "{p}"
        );
    }
    let auth = f
        .service
        .credential(
            &f.principal,
            &f.key
                .public_key()
                .fingerprint(ssh_key::HashAlg::Sha256)
                .to_string(),
        )
        .unwrap();
    assert!(
        f.service
            .files
            .trash_entries(&auth)
            .unwrap()
            .iter()
            .any(|e| e.size == 8)
    );
    f.service.revoke_key(&f.token, &f.key_id).unwrap();
    assert!(sftp.read(&read, 0, 100).await.is_err());
    assert!(
        sftp.mkdir("denied", FileAttributes::default())
            .await
            .is_err()
    );
    f.service.shutdown().await;
    assert_eq!(f.service.status(&f.token).unwrap()["running"], false);
    // Windows can complete an already queued TCP handshake briefly after close.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    while tokio::net::TcpStream::connect(address).await.is_ok() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "SFTP listener must close"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(f.service.audit.verify_chain().unwrap().valid);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn multi_megabyte_upload_keeps_one_original_and_refuses_a_stale_writer() {
    let f = Fixture::new();
    fs::write(f.game.join("large.bin"), b"original").unwrap();
    let address = f.enable().await;
    let (_client, sftp) = f.sftp(address).await;
    let h = sftp
        .open(
            "large.bin",
            OpenFlags::WRITE | OpenFlags::TRUNCATE,
            FileAttributes::default(),
        )
        .await
        .unwrap()
        .handle;
    let stale = sftp
        .open("large.bin", OpenFlags::WRITE, FileAttributes::default())
        .await
        .unwrap()
        .handle;
    let chunk = vec![42; 16 * 1024];
    for n in 0..128 {
        sftp.write(&h, n * chunk.len() as u64, chunk.clone())
            .await
            .unwrap();
    }
    assert_eq!(
        fs::metadata(f.game.join("large.bin")).unwrap().len(),
        2 * 1024 * 1024
    );
    assert!(sftp.write(&stale, 0, b"stale".to_vec()).await.is_err());
    let auth = f
        .service
        .credential(
            &f.principal,
            &f.key
                .public_key()
                .fingerprint(ssh_key::HashAlg::Sha256)
                .to_string(),
        )
        .unwrap();
    let trash = f.service.files.trash_entries(&auth).unwrap();
    assert_eq!(trash.len(), 1);
    assert_eq!(trash[0].size, 8);
    sftp.close(&h).await.unwrap();
    f.service.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn limits_role_change_expiry_and_create_only_continuation() {
    let f = Fixture::new();
    let address = f.enable().await;
    let (_client, sftp) = f.sftp(address).await;
    f.service.db.write(|c| {c.execute("UPDATE role_assignments SET revoked_at=? WHERE principal_id=?",params![f.clock.millis(),f.principal])?;c.execute("INSERT INTO principal_overrides(id,principal_id,capability,scope,effect,granted_by,created_at) VALUES(?,?,'files.create','upload.txt','allow','fixture',?)",params![uuid::Uuid::new_v4().to_string(),f.principal,f.clock.millis()])?;Ok(())}).unwrap();
    let h = sftp
        .open(
            "upload.txt",
            OpenFlags::WRITE | OpenFlags::CREATE | OpenFlags::TRUNCATE,
            FileAttributes::default(),
        )
        .await
        .unwrap()
        .handle;
    sftp.write(&h, 0, b"one".to_vec()).await.unwrap();
    sftp.write(&h, 3, b"two".to_vec()).await.unwrap();
    assert_eq!(fs::read(f.game.join("upload.txt")).unwrap(), b"onetwo");
    assert!(sftp.read(&h, 0, 20).await.is_err());
    assert!(
        sftp.write(&h, 16 * 1024 * 1024, b"x".to_vec())
            .await
            .is_err()
    );
    fs::write(f.game.join("upload.txt"), b"external edit").unwrap();
    assert!(sftp.write(&h, 0, b"stale".to_vec()).await.is_err());
    f.service
        .db
        .write(|c| {
            c.execute(
                "UPDATE sftp_keys SET expires_at=? WHERE id=?",
                params![f.clock.millis(), f.key_id],
            )?;
            Ok(())
        })
        .unwrap();
    assert!(sftp.write(&h, 0, b"expired".to_vec()).await.is_err());
    f.service.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn malformed_packet_closes_channel_and_shell_and_tunnel_are_refused() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let f = Fixture::new();
    let address = f.enable().await;
    let client = f.client(address).await;
    assert!(
        client
            .channel_open_direct_tcpip("127.0.0.1", 80, "127.0.0.1", 0)
            .await
            .is_err()
    );
    let mut channel = client.channel_open_session().await.unwrap();
    channel.exec(true, "echo forbidden").await.unwrap();
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(3), channel.wait())
            .await
            .unwrap(),
        Some(russh::ChannelMsg::Failure)
    ));
    channel.request_subsystem(true, "sftp").await.unwrap();
    let mut stream = channel.into_stream();
    stream.write_u32(128 * 1024 + 1).await.unwrap();
    let mut byte = [0];
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(3), stream.read(&mut byte))
            .await
            .unwrap()
            .unwrap(),
        0
    );
    f.service.shutdown().await;
}

#[test]
fn network_and_configuration_budgets_are_bounded() {
    let mut c = Config {
        allow_cidrs: vec!["192.0.2.0/24".parse().unwrap()],
        deny_cidrs: vec!["192.0.2.5/32".parse().unwrap()],
        ..Config::default()
    };
    assert!(c.permits("192.0.2.6".parse().unwrap()));
    assert!(!c.permits("192.0.2.5".parse().unwrap()));
    assert!(!c.permits("127.0.0.1".parse().unwrap()));
    c.limits.sessions = 0;
    assert!(c.validate().is_err());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn openssh_sftp_batch_smoke_with_pinned_host_key() {
    // This executes only the platform's existing OpenSSH client against a fresh
    // owned temp fixture, never the live agent or a user's SSH configuration.
    let f = Fixture::new();
    let address = f.enable().await;
    let key_path = f.temp.path().join("state").join("fixture_ed25519");
    fs::write(
        &key_path,
        f.key.to_openssh(ssh_key::LineEnding::LF).unwrap().as_str(),
    )
    .unwrap();
    f.service.db.directory().tighten_file(&key_path).unwrap();
    let known = f.temp.path().join("known_hosts");
    let public = f
        .service
        .host_key()
        .unwrap()
        .public_key()
        .to_openssh()
        .unwrap();
    fs::write(&known, format!("[127.0.0.1]:{} {public}\n", address.port())).unwrap();
    fs::write(
        f.temp.path().join("source.txt"),
        b"OpenSSH interoperable upload",
    )
    .unwrap();
    let batch = f.temp.path().join("batch.txt");
    fs::write(&batch,"pwd\nls\nput source.txt upload.txt\nget upload.txt downloaded.txt\nrename upload.txt renamed.txt\nrm renamed.txt\nmkdir folder\n").unwrap();
    let principal = f.principal.clone();
    let cwd = f.temp.path().to_path_buf();
    let result = tokio::task::spawn_blocking(move || {
        std::process::Command::new("sftp")
            .current_dir(cwd)
            .args(["-q", "-b"])
            .arg(batch)
            .args(["-P", &address.port().to_string(), "-i"])
            .arg(key_path)
            .arg("-o")
            .arg(format!("UserKnownHostsFile={}", known.display()))
            .args([
                "-o",
                "StrictHostKeyChecking=yes",
                "-o",
                "IdentitiesOnly=yes",
                "-o",
                "BatchMode=yes",
                "-o",
                "ConnectTimeout=5",
            ])
            .arg(format!("{principal}@127.0.0.1"))
            .output()
    })
    .await
    .unwrap();
    f.service.shutdown().await;
    let output = result.expect("OpenSSH sftp must be installed for this interoperability test");
    assert!(
        output.status.success(),
        "OpenSSH SFTP failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read(f.temp.path().join("downloaded.txt")).unwrap(),
        b"OpenSSH interoperable upload"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(f.temp.path().join("downloaded.txt"))
            .unwrap()
            .permissions()
            .mode();
        assert_ne!(mode & 0o400, 0, "download must be owner-readable");
        assert_eq!(
            mode & 0o111,
            0,
            "virtual files do not grant executable bits"
        );
    }
    assert!(f.game.join("folder").is_dir());
}
