use super::*;
use std::error::Error;

// Independent schema-1 fixture, not a serialization of application objects.
const GOLDEN: &str = r#"schema-version = 1

[paths]
state = 'private-state'
game = 'game'
scan-roots = ['game/mods', 'game/config']
launch-definition = 'private/launch.toml'

[pack]
name = 'Golden Pack'
version = 'release-2026.10'
revision = 7
minimum-revision = 3
minecraft-version = '1.21.1'
description = 'Independent host fixture'

[pack.loader]
kind = 'fabric'
version = '0.16.10'

[pack-access]
mode = 'minecraft'

[listener]
mode = 'disabled'
"#;

const TLS: &str = r#"[listener]
mode = 'tls'
bind = '127.0.0.1'
port = 8443
certificate = 'private/tls/cert.pem'
private-key = 'private/tls/key.pem'
max-connections = 64
max-request-bytes = 65536
request-timeout-seconds = 30
"#;

fn parse(text: &str, base: &Path) -> Result<HostConfiguration> {
    HostConfiguration::parse_toml(text.as_bytes(), base, Path::new("host.toml"))
}

fn refused<T>(result: Result<T>, expected: ConfigError) {
    let error = result.err().expect("configuration must refuse");
    assert_eq!(error, expected);
    assert_eq!(error.to_string(), "Host configuration failed");
    assert!(error.source().is_none());
}

fn tls_document() -> String {
    GOLDEN.replace("[listener]\nmode = 'disabled'\n", TLS)
}

fn roots_document(roots: &[String]) -> String {
    let roots = roots
        .iter()
        .map(|root| format!("'{root}'"))
        .collect::<Vec<_>>()
        .join(", ");
    GOLDEN.replace(
        "scan-roots = ['game/mods', 'game/config']",
        &format!("scan-roots = [{roots}]"),
    )
}

#[test]
fn unresolved_dispositions_are_strict_and_default_to_refuse() {
    let temp = tempfile::tempdir().unwrap();
    assert_eq!(
        parse(GOLDEN, temp.path())
            .unwrap()
            .pack()
            .unresolved_disposition(),
        UnresolvedDisposition::Refuse
    );
    for (value, expected) in [
        ("refuse", UnresolvedDisposition::Refuse),
        ("embed-public", UnresolvedDisposition::EmbedPublic),
        ("exclude-private", UnresolvedDisposition::ExcludePrivate),
    ] {
        let text = GOLDEN.replace(
            "[pack]\n",
            &format!("[pack]\nunresolved-disposition = '{value}'\n"),
        );
        assert_eq!(
            parse(&text, temp.path())
                .unwrap()
                .pack()
                .unresolved_disposition(),
            expected
        );
    }
    for value in [
        "",
        "server",
        "private",
        "embed",
        "EmbedPublic",
        "EMBED-PUBLIC",
        " embed-public",
        "exclude-private ",
        "unknown",
    ] {
        let text = GOLDEN.replace(
            "[pack]\n",
            &format!("[pack]\nunresolved-disposition = '{value}'\n"),
        );
        refused(parse(&text, temp.path()), ConfigError::InvalidDocument);
    }
    for fields in [
        "unresolved-policy = 'server'",
        "unresolved-disposition = true",
        "unresolved-disposition = ['embed-public']",
        "unresolved-disposition = {embed-public = {}}",
        "unresolved-disposition = {embed-public = []}",
        "unresolved-disposition = {refuse = {}, embed-public = {}}",
        "unresolved-disposition = 'refuse'\nunresolved-disposition = 'embed-public'",
    ] {
        let text = GOLDEN.replace("[pack]\n", &format!("[pack]\n{fields}\n"));
        refused(parse(&text, temp.path()), ConfigError::InvalidDocument);
    }
}

#[test]
fn join_description_comes_only_from_strict_trusted_config() {
    let temp = tempfile::tempdir().unwrap();
    assert!(parse(GOLDEN, temp.path()).unwrap().pack().join().is_none());
    let text = format!(
        "{GOLDEN}\n[pack.join]\naddress = 'play.example.org'\nport = 25565\ndescription = 'Player server'\n"
    );
    let config = parse(&text, temp.path()).unwrap();
    let join = config.pack().join().unwrap();
    assert_eq!(join.address(), "play.example.org");
    assert_eq!(join.port(), 25565);
    assert_eq!(join.minecraft_version(), config.pack().minecraft_version());
    assert_eq!(join.description(), Some("Player server"));
    for fields in [
        "address = 'https://example.org'\nport = 25565",
        "address = 'example.org:25565'\nport = 25565",
        "address = ' example.org'\nport = 25565",
        "address = 'example.org'\nport = 0",
        "address = 'example.org'\nport = 65536",
        "address = 'example.org'",
        "address = 'example.org'\nport = 25565\nurl = 'https://evil.org'",
        "address = 'example.org'\nport = 25565\nminecraft-version = 'client-controlled'",
        "address = 'example.org'\naddress = 'evil.org'\nport = 25565",
    ] {
        refused(
            parse(&format!("{GOLDEN}\n[pack.join]\n{fields}\n"), temp.path()),
            ConfigError::InvalidDocument,
        );
    }
}

#[test]
fn independent_toml_golden_has_explicit_paths_metadata_and_minecraft_access() {
    let temp = tempfile::tempdir().unwrap();
    let config = parse(GOLDEN, temp.path()).unwrap();
    assert_eq!(config.base(), temp.path());
    assert_eq!(config.source(), temp.path().join("host.toml"));
    assert_eq!(config.state(), temp.path().join("private-state"));
    assert_eq!(config.game(), temp.path().join("game"));
    assert_eq!(
        config.scan_roots(),
        [
            temp.path().join("game/mods"),
            temp.path().join("game/config")
        ]
    );
    assert_eq!(
        config.launch_definition(),
        temp.path().join("private/launch.toml")
    );
    assert_eq!(config.pack_access(), PackAccessMode::Minecraft);
    assert_eq!(config.listener(), &ListenerDescription::Disabled);
    let pack = config.pack();
    assert_eq!(pack.name(), "Golden Pack");
    assert_eq!(pack.version(), "release-2026.10");
    assert_eq!(pack.revision(), 7);
    assert_eq!(pack.minimum_revision(), 3);
    assert_eq!(pack.minecraft_version(), "1.21.1");
    assert_eq!(pack.description(), Some("Independent host fixture"));
    let loader = pack.loader().unwrap();
    assert_eq!(loader.kind(), LoaderKind::Fabric);
    assert_eq!(loader.version(), "0.16.10");
    assert_eq!(
        config.host_protected_paths(),
        [
            temp.path().join("host.toml"),
            temp.path().join("private/launch.toml")
        ]
    );
    assert_eq!(fs::read_dir(temp.path()).unwrap().count(), 0);
}

#[test]
fn parsing_and_observing_missing_paths_never_prepare_create_or_read_other_documents() {
    let temp = tempfile::tempdir().unwrap();
    let base = temp.path().join("absent-host-base");
    let config = parse(&tls_document(), &base).unwrap();
    assert!(!base.exists());
    let observed = config.validate_host_paths().unwrap();
    assert_eq!(
        observed.state_directory().path(),
        fs::canonicalize(temp.path())
            .unwrap()
            .join("absent-host-base/private-state")
    );
    assert!(!observed.state_directory().database_file().exists());
    assert!(!observed.state_directory().backups_dir().exists());
    let descriptor = observed.into_state_directory();
    assert!(!descriptor.path().exists());
    assert!(!base.exists());
    assert_eq!(fs::read_dir(temp.path()).unwrap().count(), 0);
}

#[test]
fn optional_pack_metadata_and_password_alternative_are_descriptions_not_admin() {
    let temp = tempfile::tempdir().unwrap();
    let text = GOLDEN
        .replace("description = 'Independent host fixture'\n", "")
        .replace("[pack.loader]\nkind = 'fabric'\nversion = '0.16.10'\n", "")
        .replace("mode = 'minecraft'", "mode = 'minecraft-or-password'");
    let config = parse(&text, temp.path()).unwrap();
    assert_eq!(config.pack_access(), PackAccessMode::MinecraftOrPassword);
    assert!(config.pack().description().is_none());
    assert!(config.pack().loader().is_none());
    for kind in ["fabric", "quilt", "forge", "neoforge"] {
        let config = parse(
            &GOLDEN.replace("kind = 'fabric'", &format!("kind = '{kind}'")),
            temp.path(),
        )
        .unwrap();
        assert!(config.pack().loader().is_some());
    }
    assert_eq!(fs::read_dir(temp.path()).unwrap().count(), 0);
}

#[test]
fn tls_listener_is_bounded_data_and_all_missing_private_documents_are_protected() {
    let temp = tempfile::tempdir().unwrap();
    let config = parse(&tls_document(), temp.path()).unwrap();
    let ListenerDescription::Tls(tls) = config.listener() else {
        panic!("TLS description expected")
    };
    assert_eq!(tls.bind(), "127.0.0.1".parse::<IpAddr>().unwrap());
    assert_eq!(tls.port(), 8443);
    assert_eq!(tls.certificate(), temp.path().join("private/tls/cert.pem"));
    assert_eq!(tls.private_key(), temp.path().join("private/tls/key.pem"));
    assert_eq!(tls.max_connections(), 64);
    assert_eq!(tls.max_request_bytes(), 65536);
    assert_eq!(tls.request_timeout_seconds(), 30);
    assert_eq!(
        config.host_protected_paths(),
        [
            temp.path().join("host.toml"),
            temp.path().join("private/launch.toml"),
            temp.path().join("private/tls/cert.pem"),
            temp.path().join("private/tls/key.pem"),
        ]
    );
    assert!(
        config
            .host_protected_paths()
            .iter()
            .all(|path| !path.exists())
    );
    assert_eq!(fs::read_dir(temp.path()).unwrap().count(), 0);
}

#[test]
fn listener_numeric_boundaries_and_literal_ipv6_are_accepted_without_binding() {
    let temp = tempfile::tempdir().unwrap();
    for (connections, bytes, timeout, port) in [(1, 1, 1, 1), (1024, 1048576, 300, 65535)] {
        let text = tls_document()
            .replace("127.0.0.1", "::1")
            .replace(
                "max-connections = 64",
                &format!("max-connections = {connections}"),
            )
            .replace(
                "max-request-bytes = 65536",
                &format!("max-request-bytes = {bytes}"),
            )
            .replace(
                "request-timeout-seconds = 30",
                &format!("request-timeout-seconds = {timeout}"),
            )
            .replace("port = 8443", &format!("port = {port}"));
        assert!(parse(&text, temp.path()).is_ok());
    }
    for (old, new) in [
        ("bind = '127.0.0.1'", "bind = 'localhost'"),
        ("bind = '127.0.0.1'", "bind = 'https://127.0.0.1'"),
        ("port = 8443", "port = 0"),
        ("max-connections = 64", "max-connections = 0"),
        ("max-connections = 64", "max-connections = 1025"),
        ("max-request-bytes = 65536", "max-request-bytes = 0"),
        ("max-request-bytes = 65536", "max-request-bytes = 1048577"),
        (
            "request-timeout-seconds = 30",
            "request-timeout-seconds = 0",
        ),
        (
            "request-timeout-seconds = 30",
            "request-timeout-seconds = 301",
        ),
        (
            "private-key = 'private/tls/key.pem'",
            "private-key = 'PRIVATE/TLS/CERT.PEM'",
        ),
        (
            "certificate = 'private/tls/cert.pem'",
            "certificate = 'host.toml'",
        ),
        (
            "private-key = 'private/tls/key.pem'",
            "private-key = 'private/launch.toml'",
        ),
    ] {
        refused(
            parse(&tls_document().replace(old, new), temp.path()),
            ConfigError::InvalidListener,
        );
    }
    for (old, new) in [
        ("port = 8443", "port = 65536"),
        ("max-connections = 64", "max-connections = 65536"),
        (
            "max-request-bytes = 65536",
            "max-request-bytes = 4294967296",
        ),
        ("port = 8443", "port = -1"),
        ("port = 8443", "port = 8443.0"),
    ] {
        refused(
            parse(&tls_document().replace(old, new), temp.path()),
            ConfigError::InvalidDocument,
        );
    }
}

#[test]
fn malformed_missing_duplicate_unknown_and_wrong_typed_documents_fail_closed() {
    let temp = tempfile::tempdir().unwrap();
    for text in [
        "",
        "not toml",
        "[]",
        "schema-version = 1",
        "[paths]\nstate = 'x'",
    ] {
        refused(parse(text, temp.path()), ConfigError::InvalidDocument);
    }
    for (old, new) in [
        ("schema-version = 1", "schema-version = '1'"),
        (
            "schema-version = 1",
            "schema-version = 1\nschema-version = 1",
        ),
        (
            "schema-version = 1",
            "schema-version = 1\n\"schema-version\" = 1",
        ),
        (
            "state = 'private-state'",
            "state = 'private-state'\nstate = 'other'",
        ),
        ("revision = 7", "revision = 7\nrevision = 8"),
        ("state = 'private-state'", "state = true"),
        (
            "scan-roots = ['game/mods', 'game/config']",
            "scan-roots = 'game/mods'",
        ),
        (
            "scan-roots = ['game/mods', 'game/config']",
            "scan-roots = [42]",
        ),
        (
            "scan-roots = ['game/mods', 'game/config']",
            "scan-roots = [['game/mods']]",
        ),
        (
            "mode = 'disabled'",
            "mode = 'disabled'\ncertificate = 'untrusted.pem'",
        ),
        ("name = 'Golden Pack'", "name = ['Golden Pack']"),
        ("kind = 'fabric'", "kind = 'unknown-loader'"),
    ] {
        refused(
            parse(&GOLDEN.replace(old, new), temp.path()),
            ConfigError::InvalidDocument,
        );
    }
    for removed in [
        "schema-version = 1\n",
        "state = 'private-state'\n",
        "revision = 7\n",
        "mode = 'minecraft'\n",
        "[listener]\nmode = 'disabled'\n",
    ] {
        refused(
            parse(&GOLDEN.replace(removed, ""), temp.path()),
            ConfigError::InvalidDocument,
        );
    }
    for text in [
        GOLDEN.replace("schema-version = 1", "schema-version = 1\nunknown = true"),
        GOLDEN.replace("[paths]", "[paths]\nunknown = true"),
        GOLDEN.replace("[pack]", "[pack]\nunknown = true"),
        GOLDEN.replace("[pack.loader]", "[pack.loader]\nunknown = true"),
        GOLDEN.replace("[pack-access]", "[pack-access]\nunknown = true"),
        format!("{GOLDEN}\n[unknown]\nmode = 'ignored'"),
        format!("{GOLDEN}\n[paths.nested]\na = {{ b = {{ c = {{ d = 'unknown' }} }} }}"),
        GOLDEN.replace("mode = 'disabled'", "mode = 'disabled'\nmode = 'disabled'"),
    ] {
        refused(parse(&text, temp.path()), ConfigError::InvalidDocument);
    }
    refused(
        HostConfiguration::parse_toml(b"\xff", temp.path(), Path::new("host.toml")),
        ConfigError::InvalidDocument,
    );
    for schema in [0, 2, 65535] {
        refused(
            parse(
                &GOLDEN.replace("schema-version = 1", &format!("schema-version = {schema}")),
                temp.path(),
            ),
            ConfigError::UnsupportedSchema,
        );
    }
    for schema in ["-1", "65536", "18446744073709551616"] {
        refused(
            parse(
                &GOLDEN.replace("schema-version = 1", &format!("schema-version = {schema}")),
                temp.path(),
            ),
            ConfigError::InvalidDocument,
        );
    }
}

#[test]
fn every_wire_table_rejects_positional_arrays_including_optional_loader_and_listener() {
    let temp = tempfile::tempdir().unwrap();
    let text = GOLDEN.replace("[paths]\nstate = 'private-state'\ngame = 'game'\nscan-roots = ['game/mods', 'game/config']\nlaunch-definition = 'private/launch.toml'", "paths = ['private-state', 'game', ['game/mods'], 'private/launch.toml']");
    refused(parse(&text, temp.path()), ConfigError::InvalidDocument);
    let text = GOLDEN.replace("[pack]\nname = 'Golden Pack'\nversion = 'release-2026.10'\nrevision = 7\nminimum-revision = 3\nminecraft-version = '1.21.1'\ndescription = 'Independent host fixture'\n\n[pack.loader]\nkind = 'fabric'\nversion = '0.16.10'", "");
    refused(
        parse(
            &format!(
                "pack = ['Golden Pack', 'release-2026.10', 7, 3, '1.21.1', 'Independent host fixture', ['fabric', '0.16.10']]\n{text}"
            ),
            temp.path(),
        ),
        ConfigError::InvalidDocument,
    );
    let text = GOLDEN.replace("[pack-access]\nmode = 'minecraft'", "");
    refused(
        parse(&format!("pack-access = ['minecraft']\n{text}"), temp.path()),
        ConfigError::InvalidDocument,
    );
    let text = GOLDEN.replace("[listener]\nmode = 'disabled'", "");
    for value in [
        "['disabled']",
        "[{ mode = 'disabled' }]",
        "'disabled'",
        "false",
    ] {
        refused(
            parse(&format!("listener = {value}\n{text}"), temp.path()),
            ConfigError::InvalidDocument,
        );
    }
    let text = GOLDEN.replace(
        "[pack.loader]\nkind = 'fabric'\nversion = '0.16.10'",
        "loader = ['fabric', '0.16.10']",
    );
    refused(parse(&text, temp.path()), ConfigError::InvalidDocument);
    // Table syntax and inline-table syntax both identify maps, never positional data.
    let text = GOLDEN.replace(
        "[pack.loader]\nkind = 'fabric'\nversion = '0.16.10'",
        "loader = { kind = 'fabric', version = '0.16.10' }",
    );
    assert!(parse(&text, temp.path()).is_ok());
}

#[test]
fn no_legacy_security_identity_launch_or_transport_authority_fallback_exists() {
    let temp = tempfile::tempdir().unwrap();
    for field in [
        "server-id = 'chosen'",
        "owner = 'chosen'",
        "roles = ['admin']",
        "executable = 'java'",
        "argv = ['-jar', 'server.jar']",
        "url = 'https://example.invalid/'",
        "password = 'secret'",
        "open = true",
        "op-admin = true",
        "loopback-admin = true",
    ] {
        refused(
            parse(&format!("{field}\n{GOLDEN}"), temp.path()),
            ConfigError::InvalidDocument,
        );
    }
    for table in [
        "security",
        "minecraft",
        "launch",
        "state",
        "administration",
        "transport",
    ] {
        refused(
            parse(&format!("{GOLDEN}\n[{table}]\nenabled = true"), temp.path()),
            ConfigError::InvalidDocument,
        );
    }
    for mode in [
        "open",
        "password",
        "password-only",
        "op",
        "loopback",
        "minecraft_or_password",
    ] {
        refused(
            parse(
                &GOLDEN.replace("mode = 'minecraft'", &format!("mode = '{mode}'")),
                temp.path(),
            ),
            ConfigError::InvalidDocument,
        );
    }
    for mode in ["http", "plain", "open", "auto-tls"] {
        refused(
            parse(
                &GOLDEN.replace("mode = 'disabled'", &format!("mode = '{mode}'")),
                temp.path(),
            ),
            ConfigError::InvalidDocument,
        );
    }
    for field in [
        "password = 'secret'",
        "admin-uuid = 'chosen'",
        "op-manage = true",
        "level = 100",
        "permissions = ['*']",
    ] {
        refused(
            parse(
                &GOLDEN.replace("[pack-access]", &format!("[pack-access]\n{field}")),
                temp.path(),
            ),
            ConfigError::InvalidDocument,
        );
    }
    assert_eq!(fs::read_dir(temp.path()).unwrap().count(), 0);
}

#[test]
fn positive_revision_order_and_exact_integer_boundaries_are_explicit_not_clamped() {
    let temp = tempfile::tempdir().unwrap();
    for (revision, minimum) in [
        (1, 1),
        (7, 7),
        (9007199254740991, 1),
        (9007199254740991, 9007199254740991),
    ] {
        let text = GOLDEN
            .replace("revision = 7\n", &format!("revision = {revision}\n"))
            .replace(
                "minimum-revision = 3",
                &format!("minimum-revision = {minimum}"),
            );
        let config = parse(&text, temp.path()).unwrap();
        assert_eq!(config.pack().revision(), revision);
        assert_eq!(config.pack().minimum_revision(), minimum);
    }
    for (revision, minimum) in [
        (0_u64, 0_u64),
        (0, 1),
        (1, 0),
        (7, 8),
        (9007199254740992, 1),
        (9007199254740992, 9007199254740992),
    ] {
        let text = GOLDEN
            .replace("revision = 7\n", &format!("revision = {revision}\n"))
            .replace(
                "minimum-revision = 3",
                &format!("minimum-revision = {minimum}"),
            );
        refused(parse(&text, temp.path()), ConfigError::InvalidRevision);
    }
    for number in ["-1", "7.0", "'7'", "18446744073709551616"] {
        refused(
            parse(
                &GOLDEN.replace("revision = 7\n", &format!("revision = {number}\n")),
                temp.path(),
            ),
            ConfigError::InvalidDocument,
        );
    }
}

#[test]
fn metadata_limits_are_utf8_byte_bounds_without_trim_or_control_rewriting() {
    let temp = tempfile::tempdir().unwrap();
    for (old, max) in [
        ("Golden Pack", 256),
        ("release-2026.10", 256),
        ("1.21.1", 256),
        ("0.16.10", 256),
        ("Independent host fixture", 2048),
    ] {
        for replacement in ["x".repeat(max), "é".repeat(max / 2)] {
            assert!(parse(&GOLDEN.replace(old, &replacement), temp.path()).is_ok());
        }
        for replacement in [
            String::new(),
            "x".repeat(max + 1),
            "é".repeat(max / 2 + 1),
            " leading".into(),
            "trailing ".into(),
        ] {
            refused(
                parse(&GOLDEN.replace(old, &replacement), temp.path()),
                ConfigError::InvalidPackMetadata,
            );
        }
    }
    refused(
        parse(
            &GOLDEN.replace("name = 'Golden Pack'", "name = \"private\\tvalue\""),
            temp.path(),
        ),
        ConfigError::InvalidPackMetadata,
    );
}

#[test]
fn document_byte_limit_is_checked_before_parsing_and_is_inclusive() {
    let temp = tempfile::tempdir().unwrap();
    let mut text = format!("{GOLDEN}\n#");
    text.push_str(&"x".repeat(65536 - text.len()));
    assert_eq!(text.len(), 65536);
    assert!(parse(&text, temp.path()).is_ok());
    text.push('x');
    refused(parse(&text, temp.path()), ConfigError::DocumentTooLarge);
    refused(
        HostConfiguration::parse_toml(
            &vec![0xff; 65537],
            Path::new("relative"),
            Path::new("source"),
        ),
        ConfigError::DocumentTooLarge,
    );
}

#[test]
fn explicit_absolute_base_and_native_absolute_values_never_use_process_cwd() {
    let temp = tempfile::tempdir().unwrap();
    refused(
        parse(GOLDEN, Path::new("relative-base")),
        ConfigError::InvalidPath,
    );
    refused(parse(GOLDEN, Path::new("")), ConfigError::InvalidPath);
    let game = temp.path().join("absolute-game");
    let game_text = game.to_str().unwrap().replace('\\', "/");
    let config = parse(
        &GOLDEN.replace("game = 'game'", &format!("game = '{game_text}'")),
        temp.path(),
    )
    .unwrap();
    assert_eq!(config.game(), game);
    let source = temp.path().join("private/source.toml");
    let config = HostConfiguration::parse_toml(GOLDEN.as_bytes(), temp.path(), &source).unwrap();
    assert_eq!(config.source(), source);
    refused(
        HostConfiguration::parse_toml(GOLDEN.as_bytes(), temp.path(), Path::new("../outside.toml")),
        ConfigError::InvalidPath,
    );
    #[cfg(windows)]
    for value in ["C:relative", "/current-drive", "//server/share/root"] {
        refused(
            parse(
                &GOLDEN.replace("game = 'game'", &format!("game = '{value}'")),
                temp.path(),
            ),
            ConfigError::InvalidPath,
        );
    }
}

#[test]
fn path_components_device_names_and_separators_fail_without_normalization() {
    let temp = tempfile::tempdir().unwrap();
    for value in [
        "",
        ".",
        "..",
        "./game",
        "game/../other",
        "game/./mods",
        "game//mods",
        "game/",
        "game\\mods",
        " game",
        "game ",
        "game.",
        "game/ leading",
        "game/child.",
        "game/child ",
        "game/file:stream",
        "game/a?b",
        "game/a*b",
        "game/a|b",
        "game/a<b",
        "game/a>b",
        "NUL",
        "nul.txt",
        "CON",
        "PRN",
        "AUX",
        "CLOCK$",
        "CONIN$",
        "CONOUT$",
        "COM0",
        "LPT0",
        "COM9.txt",
        "game/lpt9.file",
        "https://host.invalid/game",
    ] {
        refused(
            parse(
                &GOLDEN.replace("game = 'game'", &format!("game = '{value}'")),
                temp.path(),
            ),
            ConfigError::InvalidPath,
        );
    }
    for prefix in ["COM", "LPT"] {
        for digit in 0..=9 {
            let value = format!("game/{prefix}{digit}.txt");
            refused(
                parse(
                    &GOLDEN.replace("game = 'game'", &format!("game = '{value}'")),
                    temp.path(),
                ),
                ConfigError::InvalidPath,
            );
        }
    }
    refused(
        parse(
            &GOLDEN.replace("game = 'game'", "game = \"game/private\\tpath\""),
            temp.path(),
        ),
        ConfigError::InvalidPath,
    );
    for value in [
        "game/normal folder",
        "game/日本語",
        "game/COM10",
        "game/LPT10",
        "game/.hidden",
    ] {
        assert!(
            parse(
                &GOLDEN.replace("game = 'game'", &format!("game = '{value}'")),
                temp.path()
            )
            .is_ok()
        );
    }
    refused(
        parse(
            &GOLDEN.replace(
                "launch-definition = 'private/launch.toml'",
                "launch-definition = 'HOST.TOML'",
            ),
            temp.path(),
        ),
        ConfigError::InvalidPath,
    );
}

#[test]
fn bounded_scan_roots_reject_duplicates_but_allow_intentional_public_overlap() {
    let temp = tempfile::tempdir().unwrap();
    for count in [1, 8] {
        let roots = (0..count)
            .map(|i| format!("game/root-{i}"))
            .collect::<Vec<_>>();
        assert_eq!(
            parse(&roots_document(&roots), temp.path())
                .unwrap()
                .scan_roots()
                .len(),
            count
        );
    }
    refused(
        parse(&roots_document(&[]), temp.path()),
        ConfigError::InvalidDocument,
    );
    refused(
        parse(
            &roots_document(&(0..9).map(|i| format!("root-{i}")).collect::<Vec<_>>()),
            temp.path(),
        ),
        ConfigError::InvalidDocument,
    );
    for roots in [["game/mods", "game/mods"], ["game/mods", "GAME/MODS"]] {
        refused(
            parse(&roots_document(&roots.map(str::to_owned)), temp.path()),
            ConfigError::DuplicateScanRoot,
        );
    }
    let config = parse(
        &roots_document(&["game".into(), "game/mods".into()]),
        temp.path(),
    )
    .unwrap();
    assert!(config.validate_host_paths().is_ok());
    assert!(!config.state().exists());
}

#[test]
fn component_path_and_aggregate_resolved_path_budgets_are_bounded_before_native_work() {
    let temp = tempfile::tempdir().unwrap();
    assert!(
        parse(
            &GOLDEN.replace("game = 'game'", &format!("game = '{}'", "a".repeat(255))),
            temp.path()
        )
        .is_ok()
    );
    refused(
        parse(
            &GOLDEN.replace("game = 'game'", &format!("game = '{}'", "a".repeat(256))),
            temp.path(),
        ),
        ConfigError::InvalidPath,
    );
    let source = temp.path().join("host.toml");
    let long = (0..18)
        .map(|_| "x".repeat(240))
        .collect::<Vec<_>>()
        .join("/");
    refused(
        parse(
            &GOLDEN.replace("game = 'game'", &format!("game = '{long}'")),
            temp.path(),
        ),
        ConfigError::InvalidPath,
    );
    let mut base = temp.path().to_owned();
    // Eleven resolved paths each carry this explicit base. Each is individually
    // below 4096, while their aggregate exceeds 32768. No directory is created.
    for _ in 0..14 {
        base.push("x".repeat(240));
    }
    let roots = (0..8).map(|i| format!("scan-{i}")).collect::<Vec<_>>();
    refused(
        HostConfiguration::parse_toml(roots_document(&roots).as_bytes(), &base, &source),
        ConfigError::PathBudgetExceeded,
    );
    assert_eq!(fs::read_dir(temp.path()).unwrap().count(), 0);
}

#[test]
fn lexical_state_overlap_is_refused_in_both_directions_before_creation() {
    let temp = tempfile::tempdir().unwrap();
    for (state, game) in [
        ("game", "game"),
        ("game/state", "game"),
        ("private-state", "private-state/game"),
    ] {
        let text = GOLDEN
            .replace("state = 'private-state'", &format!("state = '{state}'"))
            .replace("game = 'game'", &format!("game = '{game}'"));
        let config = parse(&text, temp.path()).unwrap();
        refused(config.validate_host_paths(), ConfigError::StateOverlap);
        assert_eq!(fs::read_dir(temp.path()).unwrap().count(), 0);
    }
    for scan in ["private-state", "private-state/mods"] {
        let config = parse(&roots_document(&[scan.to_owned()]), temp.path()).unwrap();
        refused(config.validate_host_paths(), ConfigError::StateOverlap);
        assert!(!config.state().exists());
    }
    let text = roots_document(&["parent".into()])
        .replace("state = 'private-state'", "state = 'parent/private-state'");
    refused(
        parse(&text, temp.path()).unwrap().validate_host_paths(),
        ConfigError::StateOverlap,
    );
    assert_eq!(fs::read_dir(temp.path()).unwrap().count(), 0);
}

fn directory_alias(target: &Path, alias: &Path) {
    #[cfg(windows)]
    {
        let status = std::process::Command::new("cmd.exe")
            .args(["/d", "/c", "mklink", "/J"])
            .arg(alias)
            .arg(target)
            .status()
            .unwrap();
        assert!(status.success(), "create an owned test junction");
    }
    #[cfg(unix)]
    std::os::unix::fs::symlink(target, alias).unwrap();
}

fn remove_directory_alias(alias: &Path) {
    #[cfg(windows)]
    fs::remove_dir(alias).unwrap();
    #[cfg(unix)]
    fs::remove_file(alias).unwrap();
}

#[cfg(any(windows, unix))]
#[test]
fn actual_directory_alias_overlap_is_refused_with_multiple_missing_suffix_components() {
    let temp = tempfile::tempdir().unwrap();
    let real = temp.path().join("real");
    let alias = temp.path().join("alias");
    fs::create_dir(&real).unwrap();
    directory_alias(&real, &alias);
    let text = GOLDEN
        .replace(
            "state = 'private-state'",
            "state = 'real/missing/private-state'",
        )
        .replace("game = 'game'", "game = 'alias/missing'");
    let config = parse(&text, temp.path()).unwrap();
    assert!(!real.join("missing").exists());
    refused(config.validate_host_paths(), ConfigError::StateOverlap);
    assert!(!real.join("missing").exists());
    let text = roots_document(&["alias/missing".into()]).replace(
        "state = 'private-state'",
        "state = 'real/missing/private-state'",
    );
    refused(
        parse(&text, temp.path()).unwrap().validate_host_paths(),
        ConfigError::StateOverlap,
    );
    assert!(!real.join("missing").exists());
    // A linked private state spelling refuses rather than adopting a public alias.
    let text = GOLDEN.replace(
        "state = 'private-state'",
        "state = 'alias/missing/private-state'",
    );
    refused(
        parse(&text, temp.path()).unwrap().validate_host_paths(),
        ConfigError::InvalidHostPaths,
    );
    assert!(!real.join("missing").exists());
    remove_directory_alias(&alias);
}

#[test]
fn existing_nondirectory_game_scan_or_state_refuses_without_creating_state() {
    let temp = tempfile::tempdir().unwrap();
    fs::write(temp.path().join("ordinary-file"), b"unchanged").unwrap();
    for text in [
        GOLDEN.replace("game = 'game'", "game = 'ordinary-file'"),
        roots_document(&["ordinary-file".into()]),
        GOLDEN.replace("state = 'private-state'", "state = 'ordinary-file'"),
    ] {
        refused(
            parse(&text, temp.path()).unwrap().validate_host_paths(),
            ConfigError::InvalidHostPaths,
        );
    }
    assert!(!temp.path().join("private-state").exists());
    assert_eq!(
        fs::read(temp.path().join("ordinary-file")).unwrap(),
        b"unchanged"
    );
}

#[test]
fn trusted_native_loader_reuses_bounded_reader_and_has_no_missing_or_malformed_fallback() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("wrapper-config.toml");
    refused(
        HostConfiguration::load_file(&source, temp.path()),
        ConfigError::MissingDocument,
    );
    fs::write(&source, GOLDEN).unwrap();
    let loaded = HostConfiguration::load_file(&source, temp.path()).unwrap();
    let parsed = HostConfiguration::parse_toml(GOLDEN.as_bytes(), temp.path(), &source).unwrap();
    assert_eq!(loaded, parsed);
    assert!(!loaded.state().exists());
    fs::write(&source, b"not valid: private-secret-token").unwrap();
    refused(
        HostConfiguration::load_file(&source, temp.path()),
        ConfigError::InvalidDocument,
    );
    fs::write(&source, []).unwrap();
    refused(
        HostConfiguration::load_file(&source, temp.path()),
        ConfigError::InvalidDocument,
    );
    fs::write(&source, vec![b'x'; 65537]).unwrap();
    refused(
        HostConfiguration::load_file(&source, temp.path()),
        ConfigError::DocumentTooLarge,
    );
    fs::remove_file(&source).unwrap();
    fs::create_dir(&source).unwrap();
    // Windows may reject a directory at open before its regular-file test.
    let error = HostConfiguration::load_file(&source, temp.path()).unwrap_err();
    assert!(matches!(
        error,
        ConfigError::InvalidDocumentFile | ConfigError::DocumentUnavailable
    ));
    assert!(error.source().is_none());
    assert!(!temp.path().join("private-state").exists());
}

#[test]
fn hard_linked_configuration_documents_refuse_without_changing_contents() {
    let temp = tempfile::tempdir().unwrap();
    let original = temp.path().join("original.toml");
    let alias = temp.path().join("host.toml");
    fs::write(&original, GOLDEN).unwrap();
    fs::hard_link(&original, &alias).unwrap();
    refused(
        HostConfiguration::load_file(&alias, temp.path()),
        ConfigError::LinkedDocument,
    );
    assert_eq!(fs::read_to_string(&original).unwrap(), GOLDEN);
    assert_eq!(fs::read_to_string(&alias).unwrap(), GOLDEN);
    assert!(!temp.path().join("private-state").exists());
}

#[cfg(any(windows, unix))]
#[test]
fn linked_configuration_ancestor_refuses_the_shared_reader_without_touching_target() {
    let temp = tempfile::tempdir().unwrap();
    let real = temp.path().join("real");
    let alias = temp.path().join("alias");
    fs::create_dir(&real).unwrap();
    fs::write(real.join("host.toml"), GOLDEN).unwrap();
    directory_alias(&real, &alias);
    refused(
        HostConfiguration::load_file(&alias.join("host.toml"), temp.path()),
        ConfigError::LinkedDocument,
    );
    assert_eq!(fs::read_to_string(real.join("host.toml")).unwrap(), GOLDEN);
    assert!(!temp.path().join("private-state").exists());
    remove_directory_alias(&alias);
}

#[cfg(unix)]
#[test]
fn linked_and_dangling_configuration_leaves_fail_closed() {
    let temp = tempfile::tempdir().unwrap();
    fs::write(temp.path().join("real.toml"), GOLDEN).unwrap();
    let link = temp.path().join("link.toml");
    std::os::unix::fs::symlink(temp.path().join("real.toml"), &link).unwrap();
    refused(
        HostConfiguration::load_file(&link, temp.path()),
        ConfigError::LinkedDocument,
    );
    fs::remove_file(&link).unwrap();
    std::os::unix::fs::symlink(temp.path().join("absent.toml"), &link).unwrap();
    refused(
        HostConfiguration::load_file(&link, temp.path()),
        ConfigError::LinkedDocument,
    );
}

#[cfg(windows)]
#[test]
fn native_drive_verbatim_and_separator_spellings_do_not_evade_lexical_duplicate_refusal() {
    let temp = tempfile::tempdir().unwrap();
    let source = fs::canonicalize(temp.path()).unwrap().join("host.toml");
    let ordinary = temp
        .path()
        .join("host.toml")
        .to_str()
        .unwrap()
        .replace('\\', "/");
    let text = GOLDEN.replace(
        "launch-definition = 'private/launch.toml'",
        &format!("launch-definition = '{ordinary}'"),
    );
    refused(
        HostConfiguration::parse_toml(text.as_bytes(), temp.path(), &source),
        ConfigError::InvalidPath,
    );
    let absolute_scan = temp
        .path()
        .join("game/mods")
        .to_str()
        .unwrap()
        .replace('\\', "/");
    refused(
        parse(
            &roots_document(&["game/mods".into(), absolute_scan]),
            temp.path(),
        ),
        ConfigError::DuplicateScanRoot,
    );
}

#[test]
fn all_error_and_application_debug_output_discards_input_paths_metadata_and_parser_details() {
    let temp = tempfile::tempdir().unwrap();
    let secret = "do-not-log-private-value";
    let text = tls_document()
        .replace("Golden Pack", secret)
        .replace("Independent host fixture", secret)
        .replace("0.16.10", secret);
    let config = parse(&text, temp.path()).unwrap();
    for debug in [
        format!("{config:?}"),
        format!("{:?}", config.pack()),
        format!("{:?}", config.pack().loader().unwrap()),
        format!("{:?}", config.listener()),
        format!("{:?}", config.validate_host_paths().unwrap()),
    ] {
        assert!(!debug.contains(secret));
        assert!(!debug.contains(temp.path().to_str().unwrap()));
        assert!(!debug.contains("private/tls"));
    }
    let error = parse(&format!("password = '{secret}'\n{GOLDEN}"), temp.path()).unwrap_err();
    assert_eq!(error.code(), "host_config_invalid");
    assert_eq!(format!("{error}"), "Host configuration failed");
    assert!(!format!("{error:?}").contains(secret));
    assert!(error.source().is_none());
    for error in [
        ConfigError::InvalidDocument,
        ConfigError::DocumentTooLarge,
        ConfigError::UnsupportedSchema,
        ConfigError::InvalidPath,
        ConfigError::PathBudgetExceeded,
        ConfigError::DuplicateScanRoot,
        ConfigError::InvalidPackMetadata,
        ConfigError::InvalidRevision,
        ConfigError::InvalidListener,
        ConfigError::MissingDocument,
        ConfigError::LinkedDocument,
        ConfigError::InvalidDocumentFile,
        ConfigError::DocumentUnavailable,
        ConfigError::StateOverlap,
        ConfigError::InvalidHostPaths,
    ] {
        assert!(error.code().starts_with("host_config_"));
        assert_eq!(error.to_string(), "Host configuration failed");
        assert!(error.source().is_none());
    }
}

#[test]
fn whole_pack_sources_are_closed_exact_and_backend_only() {
    let temp = tempfile::tempdir().unwrap();
    let document = |fields: &str| {
        GOLDEN.replace(
            "[pack-access]",
            &format!("[pack.source]\n{fields}\n\n[pack-access]"),
        )
    };
    for provider in ["modrinth", "curseforge"] {
        let config = parse(&document(&format!("mode = 'platform'\nprovider = '{provider}'\nproject-id = 'project_1'\nversion-id = 'version-2'")), temp.path()).unwrap();
        let Some(PackSource::Platform(reference)) = config.pack_source() else {
            panic!("platform mode")
        };
        assert_eq!(reference.source().project_id(), Some("project_1"));
        assert_eq!(reference.source().version_id(), Some("version-2"));
        assert_eq!(reference.source().url(), None);
    }
    for fields in [
        "mode = 'scanned'\nroot = 'game'\ndirectories = ['mods', 'config']",
        "mode = 'scanned'\nroot = 'game'",
        "mode = 'archive'\nroot = 'game'\npath = 'packs/local.zip'",
        "mode = 'archive'\nroot = 'game'\npath = 'packs/local.MRPACK'",
    ] {
        assert!(parse(&document(fields), temp.path()).is_ok(), "{fields}");
    }
    for fields in [
        "",
        "mode = 'unknown'",
        "mode = 'Platform'",
        "mode = ' scanned'",
        "mode = ['scanned']\nroot = 'game'",
        "mode = {scanned = {}}\nroot = 'game'",
        "mode = 'scanned'",
        "mode = 'archive'\nroot = 'game'",
        "mode = 'scanned'\nroot = 'game'\npath = 'local.zip'",
        "mode = 'archive'\nroot = 'game'\npath = 'local.zip'\ndirectories = ['mods']",
        "mode = 'platform'\nprovider = 'modrinth'\nproject-id = 'p'\nversion-id = 'v'\nurl = 'https://example.test/pack'",
        "mode = 'platform'\nprovider = 'modrinth'\nproject-id = 'p'\nversion-id = 'v'\nroot = 'game'",
        "mode = 'platform'\nprovider = 'Modrinth'\nproject-id = 'p'\nversion-id = 'v'",
        "mode = 'platform'\nprovider = {modrinth = {}}\nproject-id = 'p'\nversion-id = 'v'",
        "mode = 'platform'\nprovider = 'unknown'\nproject-id = 'p'\nversion-id = 'v'",
        "mode = 'platform'\nprovider = 'modrinth'\nproject-id = 'p'",
        "mode = 'archive'\nmode = 'scanned'\nroot = 'game'\npath = 'a.zip'",
        "mode = 'scanned'\nroot = 'game'\nunknown = true",
    ] {
        refused(
            parse(&document(fields), temp.path()),
            ConfigError::InvalidDocument,
        );
    }
    for fields in [
        "mode = 'platform'\nprovider = 'modrinth'\nproject-id = 'https://example.test/p'\nversion-id = 'v'",
        "mode = 'platform'\nprovider = 'curseforge'\nproject-id = 'p'\nversion-id = 'https://example.test/v'",
        "mode = 'platform'\nprovider = 'modrinth'\nproject-id = ''\nversion-id = 'v'",
    ] {
        refused(
            parse(&document(fields), temp.path()),
            ConfigError::InvalidPackMetadata,
        );
    }
    for fields in [
        "mode = 'archive'\nroot = 'game'\npath = '../a.zip'",
        "mode = 'archive'\nroot = 'game'\npath = '/a.zip'",
        "mode = 'archive'\nroot = 'game'\npath = 'a.exe'",
        "mode = 'archive'\nroot = 'game'\npath = 'server.properties.zip/../a.zip'",
        "mode = 'scanned'\nroot = 'game'\ndirectories = []",
        "mode = 'scanned'\nroot = 'game'\ndirectories = ['mods', 'MODS']",
        "mode = 'scanned'\nroot = 'game'\ndirectories = ['mods', 'mods/sub']",
        "mode = 'scanned'\nroot = 'game'\ndirectories = ['config/a', 'Config/b']",
        "mode = 'scanned'\nroot = 'game'\ndirectories = ['../mods']",
        "mode = 'scanned'\nroot = 'game'\ndirectories = ['.vesta']",
    ] {
        refused(
            parse(&document(fields), temp.path()),
            ConfigError::InvalidPath,
        );
    }
    for value in [
        "[]",
        "true",
        "'archive'",
        "{mode = 'scanned', root = 'game', archive = {path = 'a.zip'}}",
    ] {
        let text = GOLDEN.replace("[pack]\n", &format!("[pack]\nsource = {value}\n"));
        refused(parse(&text, temp.path()), ConfigError::InvalidDocument);
    }
}

#[test]
fn selected_source_root_participates_in_state_overlap_validation() {
    let temp = tempfile::tempdir().unwrap();
    let text = GOLDEN.replace("[pack-access]", "[pack.source]\nmode = 'archive'\nroot = 'private-state'\npath = 'local.zip'\n[pack-access]");
    let config = parse(&text, temp.path()).unwrap();
    refused(config.validate_host_paths(), ConfigError::StateOverlap);
}

#[test]
fn process_supervision_and_crash_restart_are_strict_opt_in_settings() {
    let base = std::env::current_dir().unwrap();
    let default = parse(GOLDEN, &base).unwrap();
    assert_eq!(default.supervision(), Supervision::InProcess);
    assert_eq!(default.restart(), None);

    let separate = format!("{GOLDEN}\n[process]\nsupervision = 'separate'\n");
    let parsed = parse(&separate, &base).unwrap();
    assert_eq!(parsed.supervision(), Supervision::Separate);
    assert_eq!(parsed.restart(), None);

    // An empty restart table enables the recommended preset.
    let preset = format!("{separate}\n[process.restart]\n");
    assert_eq!(
        parse(&preset, &base).unwrap().restart(),
        Some(RestartSettings {
            crash_limit: 3,
            crash_window_minutes: 10,
            backoff_seconds: 10,
            max_backoff_seconds: 300,
        })
    );
    let custom = format!("{preset}crash-limit = 5\nbackoff-seconds = 30\n");
    let settings = parse(&custom, &base).unwrap().restart().unwrap();
    assert_eq!((settings.crash_limit, settings.backoff_seconds), (5, 30));

    // Crash restart without a separate supervisor, unknown keys, and loose
    // spellings all refuse.
    let in_process =
        format!("{GOLDEN}\n[process]\nsupervision = 'in-process'\n[process.restart]\n");
    refused(parse(&in_process, &base), ConfigError::InvalidDocument);
    refused(
        parse(&format!("{preset}restart-forever = true\n"), &base),
        ConfigError::InvalidDocument,
    );
    refused(
        parse(
            &format!("{GOLDEN}\n[process]\nsupervision = 'Separate'\n"),
            &base,
        ),
        ConfigError::InvalidDocument,
    );
}

#[test]
fn backup_repository_is_exactly_one_local_directory_or_s3_bucket() {
    let base = std::env::current_dir().unwrap();
    let profile = "[[backups.profile]]\nname = 'nightly'\nmode = 'stopped'\nkeep-last = 3\n";
    let local = format!("{GOLDEN}\n[backups]\nrepository = 'backups'\n{profile}");
    let parsed = parse(&local, &base).unwrap();
    let settings = parsed.backups().unwrap();
    assert!(matches!(&settings.repository, BackupRepository::Local(p) if p.ends_with("backups")));
    assert_eq!(settings.profiles[0].on_mutation, MutationPolicy::FailWorld);
    assert_eq!(settings.profiles[0].warn_seconds, 60);

    let s3 = format!(
        "{GOLDEN}\n[backups]\n[backups.s3]\nendpoint = 'https://s3.example.com'\nregion = 'us-east-1'\nbucket = 'vesta'\naccess-key-id = 'AKIDEXAMPLE'\nsecret-key-file = 'private/s3.key'\n{profile}"
    );
    let parsed = parse(&s3, &base).unwrap();
    let BackupRepository::S3(bucket) = &parsed.backups().unwrap().repository else {
        panic!("s3 repository expected");
    };
    assert!(bucket.path_style);
    assert_eq!(bucket.prefix, "");
    // The secret key file is protected from the file API like other credentials.
    assert!(
        parsed
            .host_protected_paths()
            .iter()
            .any(|p| p.ends_with("s3.key"))
    );

    let both = s3.replace("[backups]\n", "[backups]\nrepository = 'backups'\n");
    refused(parse(&both, &base), ConfigError::InvalidDocument);
    let neither = format!("{GOLDEN}\n[backups]\n{profile}");
    refused(parse(&neither, &base), ConfigError::InvalidDocument);
    let inside = local.replace("repository = 'backups'", "repository = 'game/backups'");
    refused(parse(&inside, &base), ConfigError::InvalidPath);
    let no_profiles = format!("{GOLDEN}\n[backups]\nrepository = 'backups'\n");
    refused(parse(&no_profiles, &base), ConfigError::InvalidDocument);
    refused(
        parse(&local.replace("mode = 'stopped'", "mode = 'paused'"), &base),
        ConfigError::InvalidDocument,
    );
}

#[test]
fn backup_limits_parse_sizes_and_refuse_zero_caps() {
    use super::{DEFAULT_MIN_FREE_BYTES, parse_size};
    assert_eq!(parse_size("0"), Some(0));
    assert_eq!(parse_size("500 MB"), Some(500_000_000));
    assert_eq!(parse_size("50GB"), Some(50_000_000_000));
    assert_eq!(parse_size("2 GiB"), Some(2 << 30));
    assert_eq!(parse_size("1 TiB"), Some(1 << 40));
    for bad in [
        "",
        "GB",
        "1.5 GB",
        "-1 GB",
        "10 gigs",
        "99999999999999999999 TB",
    ] {
        assert_eq!(parse_size(bad), None, "{bad}");
    }
    let base = std::env::current_dir().unwrap();
    let profile = "[[backups.profile]]\nname = 'nightly'\nmode = 'stopped'\nkeep-last = 3\n";
    let plain = format!("{GOLDEN}\n[backups]\nrepository = 'backups'\n{profile}");
    let limits = parse(&plain, &base)
        .unwrap()
        .backups()
        .unwrap()
        .limits
        .clone();
    assert_eq!(limits.min_free_bytes, DEFAULT_MIN_FREE_BYTES);
    assert_eq!(
        (limits.max_snapshots, limits.max_repository_bytes),
        (None, None)
    );
    let limited = format!(
        "{GOLDEN}\n[backups]\nrepository = 'backups'\n[backups.limits]\nmax-snapshots = 40\nmax-repository-size = '50 GB'\nmin-free-space = '0'\n{profile}max-snapshots = 10\n"
    );
    let parsed = parse(&limited, &base).unwrap();
    let settings = parsed.backups().unwrap();
    assert_eq!(settings.limits.max_snapshots, Some(40));
    assert_eq!(settings.limits.max_repository_bytes, Some(50_000_000_000));
    assert_eq!(settings.limits.min_free_bytes, 0);
    assert_eq!(settings.profiles[0].max_snapshots, Some(10));
    for bad in [
        limited.replace("max-snapshots = 40", "max-snapshots = 0"),
        limited.replace("'50 GB'", "'0'"),
        limited.replace("max-snapshots = 10", "max-snapshots = 0"),
        limited.replace("'50 GB'", "'lots'"),
        limited.replace("min-free-space", "minimum-free"),
    ] {
        refused(parse(&bad, &base), ConfigError::InvalidDocument);
    }
}

#[test]
fn advertised_pairing_endpoint_is_explicit_bounded_https() {
    let temp = tempfile::tempdir().unwrap();
    let original = tls_document();
    let config = parse(&original, temp.path()).unwrap();
    let ListenerDescription::Tls(tls) = config.listener() else {
        panic!("tls")
    };
    assert_eq!(tls.public_endpoint(), None);
    for endpoint in ["https://example.test:8443/", "https://[2001:db8::1]/vesta/"] {
        let text = original.replace(
            "mode = 'tls'",
            &format!("mode = 'tls'\npublic-endpoint = '{endpoint}'"),
        );
        let config = parse(&text, temp.path()).unwrap();
        let ListenerDescription::Tls(tls) = config.listener() else {
            panic!("tls")
        };
        assert_eq!(tls.public_endpoint(), Some(endpoint));
    }
    for endpoint in [
        " https://example.test/ ",
        &format!("https://example.test/{}", "\u{e9}".repeat(400)),
        "http://example.test/",
        "https://u:p@example.test/",
        "https://example.test/?token=x",
        "https://example.test/#x",
        "relative",
        &format!("https://example.test/{}", "a".repeat(2048)),
    ] {
        let text = original.replace(
            "mode = 'tls'",
            &format!("mode = 'tls'\npublic-endpoint = '{endpoint}'"),
        );
        assert!(parse(&text, temp.path()).is_err(), "{endpoint}");
    }
}

#[test]
fn listener_rate_defaults_and_bounds() {
    let temp = tempfile::tempdir().unwrap();
    let config = parse(&tls_document(), temp.path()).unwrap();
    let ListenerDescription::Tls(tls) = config.listener() else {
        panic!("tls")
    };
    assert_eq!(tls.peer_requests_per_minute(), 60);
    assert_eq!(tls.peer_connections_per_minute(), 120);
    assert_eq!(tls.session_requests_per_minute(), 300);
    for key in [
        "peer-requests-per-minute",
        "peer-connections-per-minute",
        "session-requests-per-minute",
    ] {
        for value in [0, 1, 60000, 60001] {
            let text =
                tls_document().replace("mode = 'tls'", &format!("mode = 'tls'\n{key} = {value}"));
            assert_eq!(
                parse(&text, temp.path()).is_ok(),
                (1..=60000).contains(&value),
                "{key}={value}"
            );
        }
    }
}
