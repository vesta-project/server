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
fn launch_preflight_checks_required_files_without_creating_state() {
    let f = Fixture::new();
    let config = f.config("minecraft", "private/launch.toml");
    validate_launch(&config).unwrap();
    assert!(!f.database_file().exists());

    let launch = NATIVE_LAUNCH.to_owned() + "required-files = ['../bin/server.jar']\n";
    std::fs::write(f.base().join("private/launch.toml"), &launch).unwrap();
    refused(validate_launch(&config), "game_invalid_launch");
    assert!(!f.database_file().exists());
    std::fs::write(f.base().join("bin/server.jar"), b"fixture").unwrap();
    validate_launch(&config).unwrap();
    std::fs::remove_file(f.base().join("bin/inert-placeholder")).unwrap();
    refused(validate_launch(&config), "game_invalid_launch");
    assert!(!f.database_file().exists());
}

#[test]
fn shared_isolation_is_local_opt_in_and_binds_the_supervisor_launch_identity() {
    let f = Fixture::new();
    let plain = f.config("minecraft", "private/launch.toml");
    // Only the local host profile changes, not executable/argv/game selection.
    let base_text = "schema-version=1\n[paths]\nstate='private-state'\ngame='game'\nscan-roots=['game/config']\nlaunch-definition='private/launch.toml'\n[pack]\nname='Hosted'\nversion='1.0.0'\nrevision=1\nminimum-revision=1\nminecraft-version='1.21.1'\n[pack-access]\nmode='minecraft'\n[listener]\nmode='disabled'\n".to_string();
    let parse = |extra: &str| {
        HostConfiguration::parse_toml(
            format!("{base_text}{extra}").as_bytes(),
            &f.base(),
            Path::new("host.toml"),
        )
        .unwrap()
    };
    let isolated = parse("[isolation]\nprofile='headless'\nruntime-directories=['bin']\n");
    let launch = |config: &HostConfiguration| {
        let checked = check_launch(config).unwrap();
        let spec = config
            .isolate_launch(checked.launch.into_launch_spec(None).unwrap())
            .unwrap();
        (checked.identity, spec)
    };
    let (before, unrestricted) = launch(&plain);
    let (after, restricted) = launch(&isolated);
    assert!(!unrestricted.is_isolated());
    assert!(restricted.is_isolated());
    assert_ne!(before, after);
    let bytes = std::fs::read(f.temp.path().join("private/launch.toml")).unwrap();
    let state = vesta_state::StateDirectory::new(plain.state(), Some(plain.game())).unwrap();
    let legacy = vesta_supervisor::launch_identity(&[
        &bytes,
        plain.base().to_string_lossy().as_bytes(),
        plain.launch_definition().to_string_lossy().as_bytes(),
        plain.game().to_string_lossy().as_bytes(),
        state.path().to_string_lossy().as_bytes(),
        plain.game().to_string_lossy().as_bytes(),
    ]);
    assert_eq!(before, legacy);
    let bad = parse("[isolation]\nprofile='headless'\nruntime-directories=['private']\n");
    assert!(validate_launch(&bad).is_err());
    assert!(!f.database_file().exists());
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

#[test]
fn host_startup_upgrades_original_v8_without_upload_table_and_purges_orphan_staging() {
    let f = Fixture::new();
    let db = f.reopen();
    let directory = db.directory().clone();
    let server = db.server_id().to_owned();
    let uploads = directory.uploads().unwrap();
    let orphan =
        vesta_state::artifacts::ArtifactName::new(&format!("{}.bin", "d".repeat(32))).unwrap();
    let file = uploads.create_new(&orphan).unwrap();
    file.sync_all().unwrap();
    drop(file);
    db.close().unwrap();

    // Restore v8: v9 adds uploads, and v10 widens the release-format constraint.
    let offline = rusqlite::Connection::open(directory.database_file()).unwrap();
    let release_sql: String = offline
        .query_row(
            "SELECT sql FROM sqlite_schema WHERE name='pack_releases'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let legacy_release_sql = release_sql.replace(", 'catalogue'", "");
    // This fixture starts at today's schema. An original v8 database cannot
    // retain v31 tables while claiming user_version=8: reapplying v31 correctly
    // refuses those duplicates. Remove their schema before that simulation.
    offline.execute_batch("DROP TABLE management_invitation_uses; DROP TABLE whitelist_sync_actor; DROP TABLE whitelist_sync_members;").unwrap();
    // These v27 tables must also be absent in the simulated original v8 database.
    offline.execute_batch("DROP TABLE pack_import_warnings; DROP TABLE pack_publication_history; DROP TABLE pack_workspace_uploads; DROP TABLE pack_workspace_published; DROP TABLE pack_workspace_receipts; DROP TABLE pack_workspace_blobs; DROP TABLE pack_workspace;").unwrap();
    offline
        .execute_batch(&format!("DROP TABLE pack_releases; {legacy_release_sql};"))
        .unwrap();
    offline
        .execute_batch("BEGIN IMMEDIATE; DROP TABLE management_invitation_ids; DROP TABLE metrics_policy; DROP TABLE metrics_activity; DROP TABLE metrics_status; DROP TABLE metrics_daily; DROP TABLE metrics_5m; DROP TABLE metrics_raw; DROP TABLE moderation_actions; DROP TRIGGER role_capabilities_builtin_no_delete; DELETE FROM role_capabilities WHERE capability='players.operator'; CREATE TRIGGER role_capabilities_builtin_no_delete BEFORE DELETE ON role_capabilities WHEN (SELECT builtin FROM roles WHERE id=OLD.role_id)=1 BEGIN SELECT RAISE(ABORT,'builtin roles are immutable'); END; DROP TABLE backup_policy; DROP TABLE backup_jobs; DROP TABLE notification_state; DROP TABLE notifications; DROP TABLE workflow_schedule; DROP TABLE run_steps; DROP TABLE workflow_runs; DROP TABLE workflows; DROP TRIGGER role_capabilities_builtin_no_delete; DELETE FROM role_capabilities WHERE capability IN ('automation.view','automation.manage','notifications.view','backups.configure'); CREATE TRIGGER role_capabilities_builtin_no_delete BEFORE DELETE ON role_capabilities WHEN (SELECT builtin FROM roles WHERE id=OLD.role_id)=1 BEGIN SELECT RAISE(ABORT,'builtin roles are immutable'); END; DROP TABLE management_invitation_issuers; DROP TABLE management_permission_receipts; DROP TABLE user_minecraft_names; DROP TABLE management_file_outcomes; DROP TABLE management_pack_outcomes; DROP TABLE pack_minimum_policy; DROP TABLE file_intents; DROP TABLE file_uploads; DROP TABLE management_device_keys; DROP TABLE management_pairing_codes; DROP TABLE management_owner_links; DROP TABLE user_minecraft_accounts; DROP TABLE user_default_roles; DROP TABLE device_password_grants; DROP TABLE access_role_sources; DELETE FROM role_capabilities WHERE role_id IN ('public','player'); DELETE FROM roles WHERE id IN ('public','player'); PRAGMA user_version=8; COMMIT;")
        .unwrap();
    assert_eq!(
        offline
            .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
            .unwrap(),
        8
    );
    assert!(
        !offline
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name='file_uploads')",
                [],
                |row| row.get::<_, bool>(0)
            )
            .unwrap()
    );
    drop(offline);
    assert_eq!(uploads.count(10).unwrap(), Some(1));

    let host = f.open("minecraft").unwrap();
    assert_eq!(
        host.database().schema_version().unwrap(),
        vesta_state::CURRENT_SCHEMA_VERSION
    );
    assert_eq!(host.database().server_id(), server);
    assert_eq!(
        host.database()
            .read(|c| Ok(
                c.query_row("SELECT count(*) FROM file_uploads", [], |row| row
                    .get::<_, i64>(0))?
            ))
            .unwrap(),
        0
    );
    assert_eq!(uploads.count(10).unwrap(), Some(0));
    assert_eq!(host.files().purge_expired_uploads().unwrap(), 0);
    assert!(matches!(
        host.shutdown(Duration::from_secs(5)).unwrap(),
        HostShutdown::Complete
    ));
}

#[test]
fn a_mismatched_launch_directory_refuses_without_creating_state_or_runtime_files() {
    for directory in ["different-game", "private-state/game", "private-state", "."] {
        let f = Fixture::new();
        std::fs::write(
            f.base().join("private/launch.toml"),
            NATIVE_LAUNCH.replace(
                "working-directory = 'game'",
                &format!("working-directory = '{directory}'"),
            ),
        )
        .unwrap();
        refused(f.open("minecraft"), "host_launch_directory_invalid");
        assert!(!f.database_file().exists());
        assert!(!f.base().join("different-game").exists());
        assert!(!f.base().join("game/operation-runner.lock").exists());
    }
}

#[test]
fn alias_spelling_of_the_launch_game_directory_is_accepted() {
    let f = Fixture::new();
    #[cfg(windows)]
    let spelling = "GAME";
    #[cfg(not(windows))]
    let spelling = "./game";
    std::fs::write(
        f.base().join("private/launch.toml"),
        NATIVE_LAUNCH.replace(
            "working-directory = 'game'",
            &format!("working-directory = '{spelling}'"),
        ),
    )
    .unwrap();
    let host = f.open("minecraft").unwrap();
    assert_eq!(host.runtime().game_snapshot().unwrap().generation, None);
    assert!(matches!(
        host.shutdown(Duration::from_secs(5)).unwrap(),
        HostShutdown::Complete
    ));
}

#[test]
fn launch_directory_errors_are_fixed_and_redacted() {
    let error = HostError::LaunchDirectory;
    assert_eq!(error.to_string(), "host_launch_directory_invalid");
    assert_eq!(
        format!("{error:?}"),
        "HostError { code: \"host_launch_directory_invalid\", .. }"
    );
}

#[test]
fn existing_mismatched_and_state_nested_launch_directories_are_unchanged_on_refusal() {
    for directory in ["different-game", "private-state/game"] {
        let f = Fixture::new();
        let launch_directory = f.base().join(directory);
        std::fs::create_dir_all(&launch_directory).unwrap();
        std::fs::write(launch_directory.join("sentinel"), b"unchanged").unwrap();
        std::fs::write(
            f.base().join("private/launch.toml"),
            NATIVE_LAUNCH.replace(
                "working-directory = 'game'",
                &format!("working-directory = '{directory}'"),
            ),
        )
        .unwrap();
        refused(f.open("minecraft"), "host_launch_directory_invalid");
        assert_eq!(
            std::fs::read(launch_directory.join("sentinel")).unwrap(),
            b"unchanged"
        );
        assert_eq!(std::fs::read_dir(&launch_directory).unwrap().count(), 1);
        if directory.starts_with("private-state") {
            assert_eq!(std::fs::read_dir(f.database_file()).unwrap().count(), 1);
        } else {
            assert!(!f.database_file().exists());
        }
    }
}

#[cfg(windows)]
#[test]
#[allow(unsafe_code)]
fn native_short_name_spelling_of_the_launch_game_directory_is_accepted_when_available() {
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    use windows_sys::Win32::Storage::FileSystem::GetShortPathNameW;
    let f = Fixture::new();
    let game = f.base().join("game");
    let wide: Vec<u16> = game.as_os_str().encode_wide().chain([0]).collect();
    let mut output = vec![0_u16; 32768];
    // SAFETY: stable NUL-terminated input and correctly sized owned output.
    let written =
        unsafe { GetShortPathNameW(wide.as_ptr(), output.as_mut_ptr(), output.len() as u32) }
            as usize;
    if written == 0 || written >= output.len() {
        eprintln!("8.3 host alias assertion skipped: GetShortPathNameW unavailable");
        return;
    }
    output.truncate(written);
    let short = std::ffi::OsString::from_wide(&output)
        .into_string()
        .unwrap();
    if !short.contains('~') {
        eprintln!("8.3 host alias assertion skipped: temporary volume supplied no short name");
        return;
    }
    std::fs::write(
        f.base().join("private/launch.toml"),
        NATIVE_LAUNCH.replace(
            "working-directory = 'game'",
            &format!("working-directory = '{short}'"),
        ),
    )
    .unwrap();
    let host = f.open("minecraft").unwrap();
    assert!(matches!(
        host.shutdown(Duration::from_secs(5)).unwrap(),
        HostShutdown::Complete
    ));
}

fn configured_pack(
    f: &Fixture,
    source: &str,
    disposition: &str,
    document_path: &str,
) -> HostConfiguration {
    let text = format!(
        "schema-version = 1\n[paths]\nstate = 'private-state'\ngame = 'game'\nscan-roots = ['unused-legacy-root']\nlaunch-definition = 'private/launch.toml'\n[pack]\nname = 'Configured'\nversion = '1.0.0'\nrevision = 77\nminimum-revision = 3\nminecraft-version = '1.21.1'\nunresolved-disposition = '{disposition}'\n[pack.source]\n{source}\n[pack-access]\nmode = 'minecraft'\n[listener]\nmode = 'disabled'\n"
    );
    HostConfiguration::parse_toml(text.as_bytes(), &f.base(), Path::new(document_path)).unwrap()
}

#[test]
fn host_prepares_and_publishes_exactly_the_configured_whole_pack_mode() {
    use crate::packs::{ArtifactFormat, MAX_ARTIFACT_BYTES, UpdatePolicy};
    use vesta_protocol::pack::{
        catalogue::{Manifest as PackManifest, PackContent},
        sources::SourceRegistry,
    };
    for (source, expected) in [
        (
            "mode = 'scanned'\nroot = 'game'\ndirectories = ['config', 'absent']",
            ArtifactFormat::Vesta,
        ),
        (
            "mode = 'archive'\nroot = 'game'\npath = 'packs/local.zip'",
            ArtifactFormat::Zip,
        ),
        (
            "mode = 'archive'\nroot = 'game'\npath = 'packs/local.mrpack'",
            ArtifactFormat::Mrpack,
        ),
        (
            "mode = 'platform'\nprovider = 'modrinth'\nproject-id = 'project'\nversion-id = 'version'",
            ArtifactFormat::Catalogue,
        ),
        (
            "mode = 'platform'\nprovider = 'curseforge'\nproject-id = '123'\nversion-id = '456'",
            ArtifactFormat::Catalogue,
        ),
    ] {
        let f = Fixture::new();
        std::fs::create_dir_all(f.base().join("game/packs")).unwrap();
        std::fs::write(
            f.base().join("game/packs/local.zip"),
            b"opaque archive, not parsed",
        )
        .unwrap();
        std::fs::write(
            f.base().join("game/packs/local.mrpack"),
            b"opaque archive, not parsed",
        )
        .unwrap();
        std::fs::write(
            f.base().join("game/config/public.txt"),
            b"retained public bytes",
        )
        .unwrap();
        std::fs::write(
            f.base().join("game/config/host.toml"),
            b"protected document",
        )
        .unwrap();
        std::fs::write(
            f.base().join("game/config/wrapper-config.toml"),
            b"protected standard secret",
        )
        .unwrap();
        let host = Host::open(
            configured_pack(&f, source, "embed-public", "game/config/host.toml"),
            Arc::new(SystemClock),
        )
        .unwrap();
        assert_eq!(principals(host.database()), 0);
        assert!(host.releases().current().unwrap().is_none());
        assert_eq!(
            host.pack_preparations().len(),
            usize::from(expected != ArtifactFormat::Catalogue)
        );
        let owner = seed_owner(host.database());
        let plan = host
            .releases()
            .plan(&owner, 0, UpdatePolicy::Optional)
            .unwrap();
        let prepared = host
            .prepare_pack(&plan, &SourceRegistry::new(Vec::new()).unwrap())
            .unwrap();
        assert_eq!(
            prepared.manifest().metadata().revision(),
            1,
            "ledger, not configured revision 77"
        );
        assert_eq!(prepared.manifest().metadata().minimum_revision(), 1);
        match prepared.manifest().content() {
            PackContent::Custom(files) => {
                assert_eq!(expected, ArtifactFormat::Vesta);
                assert_eq!(
                    files
                        .iter()
                        .map(|file| file.path().as_str())
                        .collect::<Vec<_>>(),
                    ["config/public.txt"]
                );
                assert_eq!(
                    prepared
                        .missing_directories()
                        .iter()
                        .map(RelativePath::as_str)
                        .collect::<Vec<_>>(),
                    ["absent"]
                );
            }
            PackContent::Archive(archive) => {
                assert!(matches!(
                    expected,
                    ArtifactFormat::Zip | ArtifactFormat::Mrpack
                ));
                assert_eq!(archive.size(), b"opaque archive, not parsed".len() as u64);
            }
            PackContent::Platform(reference) => {
                assert_eq!(expected, ArtifactFormat::Catalogue);
                assert_eq!(reference.source().url(), None);
            }
        }
        // No later mutable source file is reopened by publication.
        std::fs::remove_dir_all(f.base().join("game/config")).unwrap();
        std::fs::remove_dir_all(f.base().join("game/packs")).unwrap();
        let release = host
            .releases()
            .publish_prepared(&owner, &plan, &prepared, false)
            .unwrap();
        assert_eq!(release.artifact().format(), expected);
        assert_eq!(host.releases().current().unwrap().unwrap(), release);
        let loaded = host
            .releases()
            .artifacts()
            .load(release.artifact(), MAX_ARTIFACT_BYTES)
            .unwrap();
        match expected {
            ArtifactFormat::Catalogue => {
                let decoded = PackManifest::decode(loaded.bytes()).unwrap();
                assert_eq!(decoded, *prepared.manifest());
                assert!(!std::str::from_utf8(loaded.bytes()).unwrap().contains("url"));
            }
            ArtifactFormat::Zip | ArtifactFormat::Mrpack => {
                assert_eq!(loaded.bytes(), b"opaque archive, not parsed")
            }
            ArtifactFormat::Vesta => {
                let mut zip = zip::ZipArchive::new(std::io::Cursor::new(loaded.bytes())).unwrap();
                assert_eq!(zip.len(), 2);
                assert!(zip.by_name("overrides/config/public.txt").is_ok());
                assert!(zip.by_name("overrides/config/host.toml").is_err());
            }
        }
        assert_eq!(host.releases().prune(&owner).unwrap().removed(), 0);
        assert_eq!(host.runtime().game_snapshot().unwrap().generation, None);
        assert!(matches!(
            host.shutdown(Duration::from_secs(5)).unwrap(),
            HostShutdown::Complete
        ));
        std::fs::create_dir_all(f.base().join("game/config")).unwrap();
        let reopened = Host::open(
            configured_pack(&f, source, "embed-public", "game/config/host.toml"),
            Arc::new(SystemClock),
        )
        .unwrap();
        assert_eq!(reopened.releases().current().unwrap().unwrap(), release);
        assert!(matches!(
            reopened.shutdown(Duration::from_secs(5)).unwrap(),
            HostShutdown::Complete
        ));
    }
}

#[test]
fn configured_archive_uses_the_protected_reader_and_open_does_not_pin_bytes() {
    use crate::packs::UpdatePolicy;
    use vesta_protocol::pack::sources::SourceRegistry;
    for (path, protected, expected) in [
        ("protected.zip", true, "pack_path_protected"),
        ("missing.zip", false, "pack_unavailable"),
    ] {
        let f = Fixture::new();
        let host = Host::open(
            configured_pack(
                &f,
                &format!("mode = 'archive'\nroot = 'game'\npath = '{path}'"),
                "refuse",
                if protected {
                    "game/protected.zip"
                } else {
                    "host.toml"
                },
            ),
            Arc::new(SystemClock),
        )
        .unwrap();
        let owner = seed_owner(host.database());
        let plan = host
            .releases()
            .plan(&owner, 0, UpdatePolicy::Optional)
            .unwrap();
        refused(
            host.prepare_pack(&plan, &SourceRegistry::new(Vec::new()).unwrap()),
            expected,
        );
        assert!(host.releases().current().unwrap().is_none());
        assert!(matches!(
            host.shutdown(Duration::from_secs(5)).unwrap(),
            HostShutdown::Complete
        ));
    }
}

#[test]
fn platform_publication_rechecks_authority_and_revision_and_preserves_native_references() {
    use crate::packs::UpdatePolicy;
    use vesta_protocol::pack::sources::SourceRegistry;
    let f = Fixture::new();
    let host = Host::open(
        configured_pack(
            &f,
            "mode = 'platform'\nprovider = 'modrinth'\nproject-id = 'p'\nversion-id = 'v'",
            "refuse",
            "host.toml",
        ),
        Arc::new(SystemClock),
    )
    .unwrap();
    let owner = seed_owner(host.database());
    let plan = host
        .releases()
        .plan(&owner, 0, UpdatePolicy::Required)
        .unwrap();
    let prepared = host
        .prepare_pack(&plan, &SourceRegistry::new(Vec::new()).unwrap())
        .unwrap();
    host.database()
        .write(|c| {
            c.execute(
                "DELETE FROM role_assignments WHERE principal_id=?",
                [owner.principal.as_str()],
            )?;
            Ok(())
        })
        .unwrap();
    assert_eq!(
        host.releases()
            .publish_prepared(&owner, &plan, &prepared, false)
            .unwrap_err()
            .code(),
        "forbidden"
    );
    assert!(host.releases().current().unwrap().is_none());
    assert_eq!(
        std::fs::read_dir(f.base().join("private-state/artifacts"))
            .unwrap()
            .count(),
        0
    );
    host.database().write(|c| { c.execute("INSERT INTO role_assignments (id,principal_id,role_id,granted_by,created_at) VALUES ('restored',?,'owner','fixture',?)", params![owner.principal.as_str(), host.database().clock().millis()])?; Ok(()) }).unwrap();
    let release = host
        .releases()
        .publish_prepared(&owner, &plan, &prepared, false)
        .unwrap();
    assert_eq!(
        host.releases()
            .publish_prepared(&owner, &plan, &prepared, false)
            .unwrap_err()
            .code(),
        "pack_release_conflict"
    );
    let next = host
        .releases()
        .plan(&owner, release.revision(), UpdatePolicy::Optional)
        .unwrap();
    assert_eq!(
        host.releases()
            .publish_prepared(&owner, &next, &prepared, false)
            .unwrap_err()
            .code(),
        "pack_release_revision_mismatch"
    );
    assert!(matches!(
        host.shutdown(Duration::from_secs(5)).unwrap(),
        HostShutdown::Complete
    ));
}

#[test]
fn unfinished_properties_confirmation_is_invalidated_on_host_restart_without_effect() {
    let f = Fixture::new();
    let initial = b"online-mode=true\n";
    std::fs::write(f.temp.path().join("game/server.properties"), initial).unwrap();
    let host = f.open("minecraft").unwrap();
    let owner = seed_owner(host.database());
    let expected = hex::encode(Sha256::digest(initial));
    let intent = serde_json::json!({"changes":{"online-mode":false},"expected_sha256":expected});
    let issued = crate::config::ServerPropertiesEditor::new(host.files())
        .patch(
            &owner,
            "restart-properties-key01",
            serde_json::from_value(intent.clone()).unwrap(),
        )
        .unwrap();
    assert_eq!(issued["state"], "awaiting_confirmation");
    assert!(matches!(
        host.shutdown(Duration::from_secs(5)).unwrap(),
        HostShutdown::Complete
    ));
    let host = f.open("minecraft").unwrap();
    assert_eq!(
        std::fs::read(f.temp.path().join("game/server.properties")).unwrap(),
        initial
    );
    let mut approved = intent;
    approved["confirmation_token"] = issued["confirmation_token"].clone();
    assert!(matches!(
        crate::config::ServerPropertiesEditor::new(host.files()).patch(
            &owner,
            "restart-properties-key01",
            serde_json::from_value(approved).unwrap()
        ),
        Err(crate::files::FileError::ConfirmationInvalid)
    ));
    assert_eq!(host.runtime().game_snapshot().unwrap().generation, None);
    assert!(matches!(
        host.shutdown(Duration::from_secs(5)).unwrap(),
        HostShutdown::Complete
    ));
}
