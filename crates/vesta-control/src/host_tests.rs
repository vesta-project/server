//! Owned temporary configuration, state and game trees only. Nothing is launched:
//! the launch definition names an empty placeholder file and no Start is admitted.
use super::*;
use crate::files::RelativePath;
use crate::identity::{DeviceId, PrincipalId, Subject};
use rusqlite::params;
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use tempfile::TempDir;
use vesta_state::{StateDirectory, SystemClock};

const NATIVE_LAUNCH: &str = "[minecraft]\nworking-directory = 'game'\n[launch]\nkind = 'native'\nexecutable = '../bin/inert-placeholder'\nstop = 'console-stop'\n";

struct Fixture {
    temp: TempDir,
}
impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(temp.path().join("game/config")).unwrap();
        std::fs::create_dir_all(temp.path().join("private")).unwrap();
        // Satisfies launch preflight only; no Start is admitted, so it is never executed.
        std::fs::create_dir_all(temp.path().join("bin")).unwrap();
        std::fs::write(temp.path().join("bin/inert-placeholder"), b"").unwrap();
        std::fs::write(temp.path().join("private/launch.toml"), NATIVE_LAUNCH).unwrap();
        Self { temp }
    }
    fn base(&self) -> PathBuf {
        self.temp.path().to_path_buf()
    }
    fn config(&self, access: &str, launch: &str) -> HostConfiguration {
        let text = format!(
            "schema-version = 1
[paths]
state = 'private-state'
game = 'game'
scan-roots = ['game/config']
launch-definition = '{launch}'
[pack]
name = 'Hosted'
version = '1.0.0'
revision = 1
minimum-revision = 1
minecraft-version = '1.21.1'
[pack-access]
mode = '{access}'
[listener]
mode = 'disabled'
"
        );
        HostConfiguration::parse_toml(text.as_bytes(), &self.base(), Path::new("host.toml"))
            .unwrap()
    }
    fn open(&self, access: &str) -> Result<Host, HostError> {
        Host::open(
            self.config(access, "private/launch.toml"),
            Arc::new(SystemClock),
        )
    }
    fn database_file(&self) -> PathBuf {
        self.temp.path().join("private-state")
    }
    fn reopen(&self) -> StateDatabase {
        let directory =
            StateDirectory::new(self.database_file(), Some(&self.temp.path().join("game")))
                .unwrap();
        StateDatabase::open(directory, Arc::new(SystemClock)).unwrap()
    }
}

#[track_caller]
fn refused<T: fmt::Debug>(result: Result<T, HostError>, code: &str) {
    match result {
        Err(error) => assert_eq!(error.code(), code),
        Ok(value) => panic!("expected {code}, produced {value:?}"),
    }
}

fn principals(db: &StateDatabase) -> i64 {
    db.read(|c| Ok(c.query_row("SELECT count(*) FROM principals", [], |row| row.get(0))?))
        .unwrap()
}

#[test]
fn opening_composes_services_without_starting_a_game_or_bootstrapping_an_owner() {
    let f = Fixture::new();
    let host = f.open("minecraft").unwrap();
    assert!(host.pack_passwords().is_none());
    assert_eq!(host.pack_preparations().len(), 1);
    assert!(host.whitelist().is_empty());
    assert!(host.releases().current().unwrap().is_none());
    assert_eq!(principals(host.database()), 0);

    let status = host.runtime().status().unwrap();
    assert!(!status.dispatch_stopped);
    assert_eq!(status.owned_generation, None);
    assert_eq!(status.last_error_code, None);
    assert_eq!(host.runtime().game_snapshot().unwrap().generation, None);
    assert!(format!("{host:?}").starts_with("Host"));

    assert!(matches!(
        host.shutdown(Duration::from_secs(5)).unwrap(),
        HostShutdown::Complete
    ));
    // Shutdown released the database and the runner lock: it can be opened again.
    let reopened = f.reopen();
    assert_eq!(principals(&reopened), 0);
    reopened.close().unwrap();
}

#[test]
fn password_access_mode_composes_the_pack_password_service() {
    let f = Fixture::new();
    let host = f.open("minecraft-or-password").unwrap();
    assert!(host.pack_passwords().is_some());
    assert!(matches!(
        host.shutdown(Duration::from_secs(5)).unwrap(),
        HostShutdown::Complete
    ));
}

#[test]
fn a_missing_or_malformed_launch_definition_refuses_before_any_state_is_created() {
    let f = Fixture::new();
    refused(
        Host::open(
            f.config("minecraft", "private/absent.toml"),
            Arc::new(SystemClock),
        ),
        "host_launch_definition_unavailable",
    );
    std::fs::write(
        f.base().join("private/launch.toml"),
        "[minecraft]\nunknown = 1\n",
    )
    .unwrap();
    refused(f.open("minecraft"), "launch_config_invalid");
    assert!(!f.database_file().join("vesta.db").exists());
    assert!(
        !f.database_file().exists()
            || std::fs::read_dir(f.database_file())
                .unwrap()
                .next()
                .is_none()
    );
}

#[test]
fn a_launch_without_a_selected_java_refuses_and_releases_the_database() {
    let f = Fixture::new();
    std::fs::write(
        f.base().join("private/launch.toml"),
        "[minecraft]\nworking-directory = 'game'\n[launch]\nkind = 'java-jar'\njar = 'server.jar'\n",
    )
    .unwrap();
    refused(f.open("minecraft"), "launch_config_java_unavailable");
    f.reopen().close().unwrap();
}

#[test]
fn a_malformed_whitelist_refuses_and_a_valid_one_is_loaded_and_reloadable() {
    let f = Fixture::new();
    let whitelist = f.base().join("game/whitelist.json");
    std::fs::write(&whitelist, b"{\"not\":\"an array\"}").unwrap();
    refused(f.open("minecraft"), "whitelist_malformed");
    f.reopen().close().unwrap();

    std::fs::write(
        &whitelist,
        br#"[{"uuid":"550e8400-e29b-41d4-a716-446655440000","name":"Player"}]"#,
    )
    .unwrap();
    let mut host = f.open("minecraft").unwrap();
    let player =
        crate::identity::minecraft::MinecraftUuid::parse("550e8400-e29b-41d4-a716-446655440000")
            .unwrap();
    assert!(host.whitelist().contains(player));
    std::fs::write(&whitelist, b"[]").unwrap();
    host.reload_admission().unwrap();
    assert!(host.whitelist().is_empty());
    std::fs::write(&whitelist, b"[{}]").unwrap();
    refused(host.reload_admission(), "whitelist_malformed");
    // A refused reload keeps the previously loaded whitelist.
    assert!(host.whitelist().is_empty());
    assert!(matches!(
        host.shutdown(Duration::from_secs(5)).unwrap(),
        HostShutdown::Complete
    ));
}

#[test]
fn overlapping_state_and_game_roots_refuse_before_any_state_is_created() {
    let f = Fixture::new();
    let text = "schema-version = 1
[paths]
state = 'game/state'
game = 'game'
scan-roots = ['game/config']
launch-definition = 'private/launch.toml'
[pack]
name = 'Hosted'
version = '1.0.0'
revision = 1
minimum-revision = 1
minecraft-version = '1.21.1'
[pack-access]
mode = 'minecraft'
[listener]
mode = 'disabled'
";
    let config =
        HostConfiguration::parse_toml(text.as_bytes(), &f.base(), Path::new("host.toml")).unwrap();
    refused(
        Host::open(config, Arc::new(SystemClock)),
        "host_config_state_overlap",
    );
    assert!(!f.base().join("game/state").exists());
}

#[test]
fn configured_secret_documents_inside_the_game_tree_are_never_browsable() {
    let f = Fixture::new();
    std::fs::create_dir_all(f.base().join("game/private")).unwrap();
    std::fs::write(f.base().join("game/private/launch.toml"), NATIVE_LAUNCH).unwrap();
    std::fs::write(f.base().join("game/readme.txt"), b"public").unwrap();
    let host = Host::open(
        f.config("minecraft", "game/private/launch.toml"),
        Arc::new(SystemClock),
    )
    .unwrap();
    let db = host.database();
    let principal = PrincipalId::parse(&uuid::Uuid::new_v4().to_string()).unwrap();
    let device = DeviceId::parse(&uuid::Uuid::new_v4().to_string()).unwrap();
    db.write(|c| {
        let now = db.clock().millis();
        c.execute("INSERT INTO principals (id,server_id,display_name,status,created_at,updated_at) VALUES (?,?,'Fixture','active',?,?)",
            params![principal.as_str(), db.server_id(), now, now])?;
        c.execute("INSERT INTO devices (id,server_id,principal_id,label,public_key_sha256,created_at) VALUES (?,?,?,'Fixture',?,?)",
            params![device.as_str(), db.server_id(), principal.as_str(), hex::encode(Sha256::digest(device.as_str().as_bytes())), now])?;
        c.execute("INSERT INTO role_assignments (id,principal_id,role_id,granted_by,created_at) VALUES (?,?,'owner','fixture',?)",
            params![uuid::Uuid::new_v4().to_string(), principal.as_str(), now])?;
        Ok(())
    }).unwrap();
    let owner = Subject { principal, device };
    let read = |path: &str| {
        host.files()
            .read(&owner, &RelativePath::parse(path).unwrap(), 1_024)
    };
    assert_eq!(read("readme.txt").unwrap().bytes(), b"public");
    assert_eq!(
        read("private/launch.toml").unwrap_err().code(),
        "file_path_protected"
    );
    assert!(matches!(
        host.shutdown(Duration::from_secs(5)).unwrap(),
        HostShutdown::Complete
    ));
}

#[test]
fn errors_render_only_fixed_codes() {
    let error = HostError::LaunchDocument(FileError::NotFound);
    assert_eq!(error.to_string(), "host_launch_definition_unavailable");
    assert_eq!(
        format!("{error:?}"),
        "HostError { code: \"host_launch_definition_unavailable\", .. }"
    );
}

fn features(manifest: &Manifest) -> Vec<(String, Support, Option<String>)> {
    manifest
        .features
        .iter()
        .map(|feature| (feature.id.clone(), feature.support, feature.reason.clone()))
        .collect()
}

#[test]
fn discovery_reports_composed_mechanisms_bound_to_the_canonical_server() {
    let f = Fixture::new();
    let host = f.open("minecraft").unwrap();
    let manifest = host
        .discover(&DiscoveryRequest::new(vec![2, 1]).unwrap())
        .unwrap();
    assert_eq!(manifest.version, 1);
    assert_eq!(manifest.server_id, host.database().server_id());
    let not_implemented = Some("not_implemented".to_owned());
    assert_eq!(
        features(&manifest),
        [
            ("game.force_kill".to_owned(), Support::Supported, None),
            (
                "game.readiness".to_owned(),
                Support::Unsupported,
                not_implemented.clone()
            ),
            ("game.start".to_owned(), Support::Supported, None),
            ("game.stop".to_owned(), Support::Supported, None),
            (
                "supervisor.continuity".to_owned(),
                Support::Unsupported,
                not_implemented
            ),
            ("tasks.replay".to_owned(), Support::Supported, None),
        ]
    );
    let encoded = discovery::encode_manifest(&manifest).unwrap();
    assert_eq!(discovery::decode_manifest(&encoded).unwrap(), manifest);
    // Describing a mechanism started nothing.
    assert_eq!(host.runtime().game_snapshot().unwrap().generation, None);

    let unsupported = host
        .discover(&DiscoveryRequest::new(vec![9]).unwrap())
        .unwrap_err();
    assert_eq!(
        unsupported.code(),
        discovery::ErrorCode::UnsupportedProtocol
    );
    assert!(matches!(
        host.shutdown(Duration::from_secs(5)).unwrap(),
        HostShutdown::Complete
    ));
}

#[test]
fn discovery_reports_an_unsupported_stop_mechanism_honestly() {
    let f = Fixture::new();
    std::fs::write(
        f.base().join("private/launch.toml"),
        NATIVE_LAUNCH.replace("console-stop", "unsupported"),
    )
    .unwrap();
    let host = f.open("minecraft").unwrap();
    let manifest = host
        .discover(&DiscoveryRequest::new(vec![1]).unwrap())
        .unwrap();
    let stop = manifest
        .features
        .iter()
        .find(|feature| feature.id == "game.stop")
        .unwrap();
    assert_eq!(stop.support, Support::Unsupported);
    assert_eq!(stop.reason.as_deref(), Some("unsupported_stop"));
    assert!(matches!(
        host.shutdown(Duration::from_secs(5)).unwrap(),
        HostShutdown::Complete
    ));
}

#[test]
fn a_missing_launch_program_is_a_known_failure_before_any_effect() {
    let f = Fixture::new();
    std::fs::remove_file(f.base().join("bin/inert-placeholder")).unwrap();
    refused(f.open("minecraft"), "game_invalid_launch");
    // The database was released and nothing owns a runner: a later open succeeds.
    let reopened = f.reopen();
    assert_eq!(principals(&reopened), 0);
    reopened.close().unwrap();
}

fn seed_owner(db: &StateDatabase) -> Subject {
    let principal = PrincipalId::parse(&uuid::Uuid::new_v4().to_string()).unwrap();
    let device = DeviceId::parse(&uuid::Uuid::new_v4().to_string()).unwrap();
    db.write(|c| {
        let now = db.clock().millis();
        c.execute("INSERT INTO principals (id,server_id,display_name,status,created_at,updated_at) VALUES (?,?,'Fixture','active',?,?)",
            params![principal.as_str(), db.server_id(), now, now])?;
        c.execute("INSERT INTO devices (id,server_id,principal_id,label,public_key_sha256,created_at) VALUES (?,?,?,'Fixture',?,?)",
            params![device.as_str(), db.server_id(), principal.as_str(), hex::encode(Sha256::digest(device.as_str().as_bytes())), now])?;
        c.execute("INSERT INTO role_assignments (id,principal_id,role_id,granted_by,created_at) VALUES (?,?,'owner','fixture',?)",
            params![uuid::Uuid::new_v4().to_string(), principal.as_str(), now])?;
        Ok(())
    }).unwrap();
    Subject { principal, device }
}

const PLAYER: &str = "550e8400-e29b-41d4-a716-446655440000";

#[test]
fn minecraft_pack_sessions_follow_the_servers_admission_files() {
    use crate::identity::minecraft::{MinecraftUuid, VerifiedMinecraftProfile};
    use crate::pack_access::PackAccessError;
    use std::time::Instant;
    let f = Fixture::new();
    let mut host = f.open("minecraft").unwrap();
    let server = ServerUuid::parse(host.database().server_id()).unwrap();
    let player = MinecraftUuid::parse(PLAYER).unwrap();
    let proof = VerifiedMinecraftProfile::for_test(server, player);

    // Before the first server run there is no server.properties: nobody is admitted.
    assert!(host.server_properties().is_none());
    assert_eq!(host.pack_policy().unwrap().grant_minecraft(&proof), None);

    let game = f.base().join("game");
    std::fs::write(
        game.join("server.properties"),
        "white-list=true\nonline-mode=true\n",
    )
    .unwrap();
    std::fs::write(
        game.join("whitelist.json"),
        format!(r#"[{{"uuid":"{PLAYER}","name":"Player"}}]"#),
    )
    .unwrap();
    host.reload_admission().unwrap();
    let grant = host.pack_policy().unwrap().grant_minecraft(&proof).unwrap();
    let now = Instant::now();
    let (secret, _) = host.pack_sessions().issue(grant, now).unwrap();
    assert!(
        host.pack_sessions()
            .authenticate(secret.expose(), now, &host.pack_policy().unwrap())
            .is_ok()
    );

    // Banning the player in Minecraft's own file ends the pack session after reload.
    std::fs::write(
        game.join("banned-players.json"),
        format!(r#"[{{"uuid":"{PLAYER}","expires":"forever"}}]"#),
    )
    .unwrap();
    host.reload_admission().unwrap();
    assert_eq!(
        host.pack_sessions()
            .authenticate(secret.expose(), now, &host.pack_policy().unwrap()),
        Err(PackAccessError::Invalid)
    );

    // A malformed admission file refuses the reload and keeps the previous data.
    std::fs::write(game.join("server.properties"), "online-mode=maybe\n").unwrap();
    refused(host.reload_admission(), "admission_data_malformed");
    assert!(host.bans().contains(player));
    assert!(matches!(
        host.shutdown(Duration::from_secs(5)).unwrap(),
        HostShutdown::Complete
    ));
}

#[test]
fn password_pack_sessions_end_when_the_password_is_revoked() {
    use crate::identity::password::SecretPassword;
    use crate::identity::{PasswordRevision, PasswordVerificationStatus};
    use crate::pack_access::PackAccessError;
    use std::time::Instant;
    let f = Fixture::new();
    let host = f.open("minecraft-or-password").unwrap();
    let owner = seed_owner(host.database());
    let passwords = host.pack_passwords().unwrap();
    assert_eq!(host.pack_policy().unwrap().password, None);
    let password = SecretPassword::try_from("correct horse battery".to_owned()).unwrap();
    let metadata = passwords
        .set(&owner, PasswordRevision::ABSENT, &password)
        .unwrap();

    let verification = passwords.verify(b"correct horse battery".to_vec()).unwrap();
    assert_eq!(verification.status(), PasswordVerificationStatus::Match);
    let grant = host
        .pack_policy()
        .unwrap()
        .grant_password(&verification)
        .unwrap();
    let wrong = passwords.verify(b"wrong horse".to_vec()).unwrap();
    assert_eq!(host.pack_policy().unwrap().grant_password(&wrong), None);

    let now = Instant::now();
    let (secret, _) = host.pack_sessions().issue(grant, now).unwrap();
    assert!(
        host.pack_sessions()
            .authenticate(secret.expose(), now, &host.pack_policy().unwrap())
            .is_ok()
    );
    passwords.revoke(&owner, metadata.revision()).unwrap();
    assert_eq!(
        host.pack_sessions()
            .authenticate(secret.expose(), now, &host.pack_policy().unwrap()),
        Err(PackAccessError::Invalid)
    );
    assert!(matches!(
        host.shutdown(Duration::from_secs(5)).unwrap(),
        HostShutdown::Complete
    ));
}

#[test]
fn malformed_admission_data_refuses_startup_and_releases_the_database() {
    let f = Fixture::new();
    std::fs::write(f.base().join("game/server.properties"), "white-list=yes\n").unwrap();
    refused(f.open("minecraft"), "admission_data_malformed");
    std::fs::write(
        f.base().join("game/server.properties"),
        "white-list=false\n",
    )
    .unwrap();
    std::fs::write(f.base().join("game/banned-players.json"), b"{}").unwrap();
    refused(f.open("minecraft"), "admission_data_malformed");
    f.reopen().close().unwrap();
}

#[test]
fn network_facades_are_send_sync_share_sessions_and_observe_atomic_admission_reload() {
    fn send_sync<T: Send + Sync>() {}
    send_sync::<NetworkHost>();
    let f = Fixture::new();
    let mut host = f.open("minecraft").unwrap();
    let network = host.network_handle();
    let clone = network.clone();
    let player =
        crate::identity::minecraft::MinecraftUuid::parse("12345678-1234-4234-8234-123456789abc")
            .unwrap();
    assert!(std::ptr::eq(network.sessions(), host.pack_sessions()));
    assert!(std::ptr::eq(clone.sessions(), network.sessions()));
    assert!(std::ptr::eq(clone.releases(), host.releases()));
    assert!(!network.with_policy(|p| p.admits(player)).unwrap());
    std::fs::write(
        f.base().join("game/server.properties"),
        "online-mode=true\nwhite-list=false\n",
    )
    .unwrap();
    host.reload_admission().unwrap();
    assert!(network.with_policy(|p| p.admits(player)).unwrap());
    assert!(clone.with_policy(|p| p.admits(player)).unwrap());
    std::fs::write(
        f.base().join("game/banned-players.json"),
        b"[{\"uuid\":\"12345678-1234-4234-8234-123456789abc\",\"name\":\"Fixture\"}]",
    )
    .unwrap();
    host.reload_admission().unwrap();
    assert!(!network.with_policy(|p| p.admits(player)).unwrap());
    std::fs::write(f.base().join("game/banned-players.json"), b"not JSON").unwrap();
    assert!(host.reload_admission().is_err());
    assert!(!clone.with_policy(|p| p.admits(player)).unwrap());
    let request = DiscoveryRequest::new(vec![1]).unwrap();
    assert_eq!(
        network.discover(&request).unwrap(),
        host.discover(&request).unwrap()
    );
    drop(network);
    drop(clone);
    assert!(matches!(
        host.shutdown(Duration::from_secs(5)).unwrap(),
        HostShutdown::Complete
    ));
}

#[test]
fn owned_serving_refresh_ends_minecraft_sessions_and_preserves_malformed_snapshot() {
    use crate::identity::minecraft::{MinecraftUuid, VerifiedMinecraftProfile};
    use crate::pack_access::PackAccessError;
    use std::time::Instant;
    let f = Fixture::new();
    let game = f.base().join("game");
    std::fs::write(
        game.join("server.properties"),
        "online-mode=true\nwhite-list=true\n",
    )
    .unwrap();
    let allow = format!(r#"[{{"uuid":"{PLAYER}","name":"Player"}}]"#);
    std::fs::write(game.join("whitelist.json"), &allow).unwrap();
    let host = f.open("minecraft").unwrap();
    let network = host.network_handle();
    let player = MinecraftUuid::parse(PLAYER).unwrap();
    let proof = VerifiedMinecraftProfile::for_test(
        ServerUuid::parse(host.database().server_id()).unwrap(),
        player,
    );
    let refresh = network.start_admission_refresh().unwrap();
    let wait = |admitted: bool| {
        let deadline = Instant::now() + Duration::from_secs(7);
        while network.with_policy(|p| p.admits(player)).unwrap() != admitted {
            assert!(
                Instant::now() < deadline,
                "refresh did not observe local policy edit"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    };
    let grant = network
        .with_policy(|p| p.grant_minecraft(&proof))
        .unwrap()
        .unwrap();
    let (secret, _) = network.sessions().issue(grant, Instant::now()).unwrap();
    std::fs::write(game.join("whitelist.json"), b"[]").unwrap();
    wait(false);
    assert_eq!(
        network
            .with_policy(|p| network
                .sessions()
                .authenticate(secret.expose(), Instant::now(), p))
            .unwrap(),
        Err(PackAccessError::Invalid)
    );
    std::fs::write(game.join("whitelist.json"), &allow).unwrap();
    wait(true);
    let grant = network
        .with_policy(|p| p.grant_minecraft(&proof))
        .unwrap()
        .unwrap();
    let (secret, _) = network.sessions().issue(grant, Instant::now()).unwrap();
    std::fs::write(
        game.join("banned-players.json"),
        format!(r#"[{{"uuid":"{PLAYER}","expires":"forever"}}]"#),
    )
    .unwrap();
    wait(false);
    assert_eq!(
        network
            .with_policy(|p| network
                .sessions()
                .authenticate(secret.expose(), Instant::now(), p))
            .unwrap(),
        Err(PackAccessError::Invalid)
    );
    std::fs::write(game.join("banned-players.json"), b"malformed").unwrap();
    std::thread::sleep(Duration::from_millis(2200));
    assert!(!network.with_policy(|p| p.admits(player)).unwrap());
    let started = Instant::now();
    drop(refresh);
    assert!(started.elapsed() < Duration::from_secs(1));
    drop(network);
    assert!(matches!(
        host.shutdown(Duration::from_secs(5)).unwrap(),
        HostShutdown::Complete
    ));
}

#[test]
fn host_startup_reconciles_pending_file_rows_before_runtime_admission() {
    let f = Fixture::new();
    let host = f.open("minecraft").unwrap();
    assert_eq!(host.files().reconcile_pending().unwrap(), 0);
    host.database().write(|c| {
        c.execute("INSERT INTO file_trash (id,server_id,path,sha256,size,reason,principal_id,device_id,status,effect_sha256,unix_mode,readonly,trashed_at,expires_at) VALUES (?,?,?, ?,3,'delete',?,?,'pending',NULL,NULL,0,0,1)", params!["a".repeat(32),host.database().server_id(),"config/missing.txt","0".repeat(64),"aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa","bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb"])?;
        Ok(())
    }).unwrap();
    assert!(matches!(
        host.shutdown(Duration::from_secs(5)).unwrap(),
        HostShutdown::Complete
    ));
    let host = f.open("minecraft").unwrap();
    let status: String = host
        .database()
        .read(|c| {
            Ok(c.query_row(
                "SELECT status FROM file_trash WHERE id=?",
                ["a".repeat(32)],
                |r| r.get(0),
            )?)
        })
        .unwrap();
    assert_eq!(
        status, "recovery-required",
        "a missing copy cannot be silently completed or deleted"
    );
    assert_eq!(host.files().reconcile_pending().unwrap(), 0);
    assert_eq!(host.runtime().game_snapshot().unwrap().generation, None);
    assert!(matches!(
        host.shutdown(Duration::from_secs(5)).unwrap(),
        HostShutdown::Complete
    ));
}
