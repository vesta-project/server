//! Windows-owned tests for explicit trusted pack preparation.
//!
//! Digests are frozen literals computed independently of the code under test, from the
//! exact byte strings written as constants below.
//!
//! These APIs prepare bytes for a later, separately authorized route. Nothing here is a
//! route, a session, or permission to publish, and no assertion grants authority.
use super::{
    MAX_PINNED_BYTES, MAX_PINNED_FILE_BYTES, MAX_SNAPSHOT_FILES, MAX_SNAPSHOT_SCAN, PackError,
    PackPreparation, SnapshotLimits,
};
use crate::files::{FileError, RelativePath};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tempfile::TempDir;
use vesta_protocol::pack::catalogue::{
    ArchiveFormat, CatalogueError, FileSource, ManifestMetadata, PackContent, PackFile, Platform,
    RelativePath as CataloguePath, SourceKind,
};
use vesta_protocol::pack::sources::{KnownSource, SourceRegistry};
use vesta_state::{Clock, StateDatabase, StateDirectory};

const ALPHA_BYTES: &[u8] = b"mods/alpha.jar";
const ALPHA_HASH: &str = "0eea8ba7784978ed99f110378e553f21b66a44a80ec3fc4849bee74dd237012b";

const B_TXT_BYTES: &[u8] = b"config/b.txt";
const B_TXT_HASH: &str = "9921a295a37e55e89b5f62a63a3732cb4f41ee4abec7a26af7d2433a12de5b66";

const EMPTY_HASH: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

const KNOWN_BYTES: &[u8] = b"known-source-file-bytes";
const KNOWN_HASH: &str = "e4a02243add1acdbd6e286c9e12f89f1e2c742090ccd440c905fe23b07127e71";

const STALE_BYTES: &[u8] = b"stale-source-file-bytes";

struct FixedClock;
impl Clock for FixedClock {
    fn millis(&self) -> i64 {
        1_791_115_200_000
    }
}

/// A real temporary tree plus a real state database. No fixture authority is created:
/// this is trusted local preparation only, and the directory exclusions the host must
/// apply are passed in explicitly by each test.
struct Fixture {
    // Held only to own the temporary tree's lifetime on disk.
    _temp: TempDir,
    game: PathBuf,
    state: PathBuf,
    db: StateDatabase,
}
impl Fixture {
    fn open() -> Self {
        let temp = tempfile::tempdir().expect("a temporary tree");
        let game = temp.path().join("game");
        std::fs::create_dir(&game).expect("a game directory");
        let state = temp.path().join("private-state");
        let directory =
            StateDirectory::new(&state, Some(&game)).expect("a state directory beside the game");
        let db = StateDatabase::open(directory, Arc::new(FixedClock)).expect("a state database");
        Self {
            _temp: temp,
            game,
            state,
            db,
        }
    }
    fn write(&self, relative: &str, bytes: &[u8]) {
        let path = self.game.join(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("a parent directory");
        }
        std::fs::write(path, bytes).expect("fixture bytes are written");
    }
    fn mkdir(&self, relative: &str) -> PathBuf {
        let path = self.game.join(relative);
        std::fs::create_dir_all(&path).expect("a fixture directory");
        path
    }
    fn db_path(&self) -> &Path {
        &self.state
    }
}

fn limits(file_bytes: usize, total_bytes: usize, files: usize, scanned: usize) -> SnapshotLimits {
    SnapshotLimits::new(file_bytes, total_bytes, files, scanned)
        .unwrap_or_else(|error| panic!("limits must be valid: {error:?}"))
}

fn preparation(fixture: &Fixture) -> PackPreparation {
    PackPreparation::open(
        &fixture.db,
        &fixture.game,
        &[],
        limits(1_024, 1_024, 32, 256),
    )
    .unwrap_or_else(|error| panic!("a plain root opens: {error:?}"))
}

#[test]
fn unresolved_refuses_by_default_and_public_embedding_requires_explicit_selection() {
    use crate::config::UnresolvedDisposition;
    let fixture = Fixture::open();
    fixture.write("config/unresolved.txt", B_TXT_BYTES);
    let prepare = preparation(&fixture);
    assert!(matches!(
        prepare.snapshot(metadata(), &empty_registry()),
        Err(PackError::UnresolvedFile)
    ));
    let registry = SourceRegistry::new(vec![
        KnownSource::new(
            "config/unresolved.txt",
            B_TXT_HASH,
            FileSource::platform(Platform::Curseforge, "1", "2", None).unwrap(),
        )
        .unwrap(),
    ])
    .unwrap();
    let known = prepare.snapshot(metadata(), &registry).unwrap();
    assert_eq!(entries(&known), ["config/unresolved.txt"]);
    assert_eq!(
        file_in(&known, "config/unresolved.txt").source().kind(),
        SourceKind::Platform
    );
    let snapshot = prepare
        .snapshot_with_disposition(
            UnresolvedDisposition::EmbedPublic,
            metadata(),
            &empty_registry(),
        )
        .unwrap();
    assert_eq!(entries(&snapshot), ["config/unresolved.txt"]);
    assert_eq!(
        snapshot.bytes(&catalogue_path("config/unresolved.txt")),
        Some(B_TXT_BYTES)
    );
    assert!(snapshot.private_files().is_empty());
    let configured =
        preparation(&fixture).with_unresolved_disposition(UnresolvedDisposition::ExcludePrivate);
    let excluded = configured.snapshot(metadata(), &empty_registry()).unwrap();
    assert!(entries(&excluded).is_empty());
    assert_eq!(excluded.private_files().len(), 1);
}

#[test]
fn excluded_private_files_are_reported_but_never_retained_or_built_or_indexed() {
    use crate::config::{HostConfiguration, UnresolvedDisposition};
    use std::io::{Cursor, Read};
    let fixture = Fixture::open();
    fixture.write("config/private.txt", B_TXT_BYTES);
    fixture.write("mods/provider.jar", KNOWN_BYTES);
    let registry = SourceRegistry::new(vec![
        KnownSource::new(
            "mods/provider.jar",
            KNOWN_HASH,
            FileSource::platform(Platform::Curseforge, "1", "2", None).unwrap(),
        )
        .unwrap(),
    ])
    .unwrap();
    let snapshot = preparation(&fixture)
        .snapshot_with_disposition(UnresolvedDisposition::ExcludePrivate, metadata(), &registry)
        .unwrap();
    assert_eq!(entries(&snapshot), ["mods/provider.jar"]);
    assert_eq!(snapshot.total_bytes(), KNOWN_BYTES.len());
    assert!(
        snapshot
            .bytes(&catalogue_path("config/private.txt"))
            .is_none()
    );
    let private = &snapshot.private_files()[0];
    assert_eq!(private.path().as_str(), "config/private.txt");
    assert_eq!(private.sha256().to_hex(), B_TXT_HASH);
    assert_eq!(private.size(), B_TXT_BYTES.len() as u64);
    assert_eq!(format!("{private:?}"), "PrivateFileEntry[<redacted>]");
    let config = HostConfiguration::parse_toml(
        br#"schema-version = 1
[paths]
state = 'state'
game = 'game'
scan-roots = ['game']
launch-definition = 'private/launch.toml'
[pack]
name = 'prepared'
version = '1.0.0'
revision = 4
minimum-revision = 2
minecraft-version = '1.21.1'
unresolved-disposition = 'exclude-private'
[pack-access]
mode = 'minecraft'
[listener]
mode = 'disabled'
"#,
        fixture._temp.path(),
        Path::new("host.toml"),
    )
    .unwrap();
    let artifact =
        super::build_vesta(&snapshot, config.pack(), super::VestaBuildLimits::default()).unwrap();
    assert_eq!(artifact.private_files(), snapshot.private_files());
    let mut archive = zip::ZipArchive::new(Cursor::new(artifact.bytes())).unwrap();
    assert_eq!(archive.len(), 1); // Provider-only public content needs just the index.
    let mut index = String::new();
    archive
        .by_name(vesta_protocol::pack::INDEX_NAME)
        .unwrap()
        .read_to_string(&mut index)
        .unwrap();
    assert!(!index.contains("private.txt"));
    assert!(!index.contains(B_TXT_HASH));
    assert!(!index.contains("private_files"));
    assert!(
        !artifact
            .bytes()
            .windows(B_TXT_BYTES.len())
            .any(|bytes| bytes == B_TXT_BYTES)
    );
    assert!(archive.by_name("overrides/config/private.txt").is_err());
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&index).unwrap()["files"][0]["path"],
        "mods/provider.jar"
    );
}

#[test]
fn private_exclusion_keeps_scan_file_and_byte_budgets_and_stale_registry_refusal() {
    use crate::config::UnresolvedDisposition;
    let fixture = Fixture::open();
    fixture.write("config/a.txt", B_TXT_BYTES);
    fixture.write("config/b.txt", B_TXT_BYTES);
    let prepare =
        PackPreparation::open(&fixture.db, &fixture.game, &[], limits(64, 64, 1, 256)).unwrap();
    assert!(matches!(
        prepare.snapshot_with_disposition(
            UnresolvedDisposition::ExcludePrivate,
            metadata(),
            &empty_registry()
        ),
        Err(PackError::TooLarge)
    ));
    let prepare = PackPreparation::open(
        &fixture.db,
        &fixture.game,
        &[],
        limits(B_TXT_BYTES.len(), B_TXT_BYTES.len(), 32, 256),
    )
    .unwrap();
    assert!(matches!(
        prepare.snapshot_with_disposition(
            UnresolvedDisposition::ExcludePrivate,
            metadata(),
            &empty_registry()
        ),
        Err(PackError::TooLarge)
    ));
    let registry = SourceRegistry::new(vec![
        KnownSource::new(
            "config/a.txt",
            KNOWN_HASH,
            FileSource::platform(Platform::Curseforge, "1", "2", None).unwrap(),
        )
        .unwrap(),
    ])
    .unwrap();
    std::fs::remove_file(fixture.game.join("config/b.txt")).unwrap();
    for disposition in [
        UnresolvedDisposition::Refuse,
        UnresolvedDisposition::EmbedPublic,
        UnresolvedDisposition::ExcludePrivate,
    ] {
        assert_catalogue_error(
            preparation(&fixture).snapshot_with_disposition(disposition, metadata(), &registry),
            CatalogueError::InvalidHash,
        );
    }
}

fn metadata() -> ManifestMetadata {
    ManifestMetadata::new("prepared", "1.0.0", 4, 2).expect("valid metadata")
}

fn empty_registry() -> SourceRegistry {
    SourceRegistry::new(Vec::new()).expect("an explicitly empty registry is valid")
}

fn catalogue_path(text: &str) -> CataloguePath {
    CataloguePath::parse(text)
        .unwrap_or_else(|error| panic!("{text} is a catalogue path: {error:?}"))
}

fn relative(text: &str) -> RelativePath {
    RelativePath::parse(text).unwrap_or_else(|error| panic!("{text} is a relative path: {error:?}"))
}

fn entries(snapshot: &super::PreparedSnapshot) -> Vec<&str> {
    let PackContent::Custom(files) = snapshot.manifest().content() else {
        panic!("a file scan produces custom content");
    };
    files.iter().map(|file| file.path().as_str()).collect()
}

fn file_in(snapshot: &super::PreparedSnapshot, name: &str) -> PackFile {
    let PackContent::Custom(files) = snapshot.manifest().content() else {
        panic!("a file scan produces custom content");
    };
    files
        .iter()
        .find(|file| file.path().as_str() == name)
        .unwrap_or_else(|| panic!("{name} is catalogued"))
        .clone()
}

#[track_caller]
fn assert_pack_error<T: std::fmt::Debug>(result: Result<T, PackError>, expected: PackError) {
    match result {
        Err(actual) => assert_eq!(actual, expected),
        Ok(value) => panic!("expected {expected:?}, produced {value:?}"),
    }
}

#[track_caller]
fn assert_catalogue_error<T: std::fmt::Debug>(
    result: Result<T, PackError>,
    expected: CatalogueError,
) {
    match result {
        Err(PackError::Catalogue(actual)) => assert_eq!(actual, expected),
        Err(other) => panic!("expected catalogue {expected:?}, produced {other:?}"),
        Ok(value) => panic!("expected catalogue {expected:?}, produced {value:?}"),
    }
}

#[test]
fn trusted_host_snapshot_builds_the_frozen_hybrid_index_from_only_pinned_public_bytes() {
    use crate::config::HostConfiguration;
    use std::io::{Cursor, Read};
    let fixture = Fixture::open();
    fixture.write("config/options.txt", b"embedded");
    fixture.write("mods/curseforge.jar", b"known");
    fixture.write("mods/modrinth.jar", b"known");
    fixture.write("mods/host-secret.toml", b"private fixture secret");
    let document = br#"schema-version = 1
[paths]
state = 'private-state'
game = 'game'
scan-roots = ['game/mods', 'game/config']
launch-definition = 'game/mods/host-secret.toml'
[pack]
name = 'Java interoperability fixture'
version = '1.2.3'
revision = 4
minimum-revision = 2
minecraft-version = '1.21.1'
[pack.loader]
kind = 'fabric'
version = '0.16.0'
[pack-access]
mode = 'minecraft'
[listener]
mode = 'disabled'
"#;
    let host =
        HostConfiguration::parse_toml(document, fixture._temp.path(), Path::new("host.toml"))
            .unwrap();
    host.validate_host_paths().unwrap();
    let pack = host.pack();
    let metadata = ManifestMetadata::new(
        pack.name(),
        pack.version(),
        pack.revision(),
        pack.minimum_revision(),
    )
    .unwrap();
    let protected: Vec<_> = host
        .host_protected_paths()
        .iter()
        .map(PathBuf::as_path)
        .collect();
    let preparation = PackPreparation::open(
        &fixture.db,
        host.game(),
        &protected,
        limits(1_024, 1_024, 32, 256),
    )
    .unwrap();
    // The retained Java test's literal b"known" has these independently checked
    // fixture hashes; provider bodies are inert text, never executable JARs.
    let provider_hash = "7117fff2d0fd294462b3c802b7cb8753579f23f3946b99cf55f38e873f013f10";
    let sources = SourceRegistry::new(vec![
        KnownSource::new(
            "mods/curseforge.jar",
            provider_hash,
            FileSource::platform(
                Platform::Curseforge,
                "123456",
                "1234567",
                Some("https://edge.forgecdn.net/files/1234/567/curseforge.jar"),
            )
            .unwrap(),
        )
        .unwrap(),
        KnownSource::new(
            "mods/modrinth.jar",
            provider_hash,
            FileSource::platform(
                Platform::Modrinth,
                "Abcd1234",
                "Efgh5678",
                Some("https://cdn.modrinth.com/data/Abcd1234/versions/Efgh5678/modrinth.jar"),
            )
            .unwrap(),
        )
        .unwrap(),
    ])
    .unwrap();
    let snapshot = preparation
        .snapshot_with_disposition(
            crate::config::UnresolvedDisposition::EmbedPublic,
            metadata,
            &sources,
        )
        .unwrap();
    assert_eq!(
        entries(&snapshot),
        [
            "config/options.txt",
            "mods/curseforge.jar",
            "mods/modrinth.jar"
        ]
    );
    let build_limits =
        super::VestaBuildLimits::new(1_024 * 1_024, 68 * 1_024 * 1_024, 4_097).unwrap();
    let artifact = super::build_vesta(&snapshot, pack, build_limits).unwrap();
    assert_eq!(artifact.format(), "vesta");
    assert_eq!(artifact.size(), artifact.bytes().len() as u64);
    let mut archive = zip::ZipArchive::new(Cursor::new(artifact.bytes())).unwrap();
    assert_eq!(
        archive.len(),
        2,
        "providers remain references, not embedded downloads"
    );
    let mut embedded = Vec::new();
    archive
        .by_name("overrides/config/options.txt")
        .unwrap()
        .read_to_end(&mut embedded)
        .unwrap();
    assert_eq!(embedded, b"embedded");
    let mut index = Vec::new();
    archive
        .by_name("vesta.index.json")
        .unwrap()
        .read_to_end(&mut index)
        .unwrap();
    let actual: serde_json::Value = serde_json::from_slice(&index).unwrap();
    let mut expected: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../core/src/test/resources/vesta-builder-v1.json"
    ))
    .unwrap();
    // Compare the full independent fixture, including both provider hashes.
    // Only ordinary embedded SHA-1 is omitted: snapshot preparation promises
    // SHA-256, while SHA-1 remains optional for embedded files on the shared wire.
    expected["files"][0]["hashes"]
        .as_object_mut()
        .unwrap()
        .remove("sha1");
    assert_eq!(actual, expected);
    drop(archive);
    fixture.write(
        "config/options.txt",
        b"later replacement must not be served under the old digest",
    );
    fixture.write("mods/curseforge.jar", b"later provider replacement");
    std::fs::remove_file(fixture.game.join("mods/modrinth.jar")).unwrap();
    let repeated = super::build_vesta(&snapshot, pack, build_limits).unwrap();
    assert_eq!(repeated.bytes(), artifact.bytes());
    assert_eq!(repeated.sha256(), artifact.sha256());
    assert_eq!(
        std::fs::read(fixture.game.join("mods/host-secret.toml")).unwrap(),
        b"private fixture secret"
    );
}

/// A scan of mods and config is deterministic, and every digest is the digest of the
/// exact bytes the file holds.
#[test]
fn a_scan_is_deterministic_and_hashes_the_retained_bytes() {
    let fixture = Fixture::open();
    fixture.write("mods/alpha.jar", ALPHA_BYTES);
    fixture.write("config/b.txt", B_TXT_BYTES);
    fixture.write("config/empty", b"");
    let preparation = preparation(&fixture);

    let first = preparation
        .snapshot_with_disposition(
            crate::config::UnresolvedDisposition::EmbedPublic,
            metadata(),
            &empty_registry(),
        )
        .expect("the first scan succeeds");
    let second = preparation
        .snapshot_with_disposition(
            crate::config::UnresolvedDisposition::EmbedPublic,
            metadata(),
            &empty_registry(),
        )
        .expect("a repeated scan succeeds");

    assert_eq!(
        entries(&first),
        ["config/b.txt", "config/empty", "mods/alpha.jar"]
    );

    assert_eq!(
        file_in(&first, "mods/alpha.jar").sha256().to_hex(),
        ALPHA_HASH
    );
    assert_eq!(
        file_in(&first, "config/b.txt").sha256().to_hex(),
        B_TXT_HASH
    );
    assert_eq!(
        file_in(&first, "config/empty").sha256().to_hex(),
        EMPTY_HASH
    );
    assert_eq!(file_in(&first, "config/empty").size(), 0);

    assert_eq!(first.total_bytes(), ALPHA_BYTES.len() + B_TXT_BYTES.len());
    assert_eq!(
        first.bytes(&catalogue_path("mods/alpha.jar")),
        Some(ALPHA_BYTES)
    );
    assert_eq!(
        first.bytes(&catalogue_path("config/b.txt")),
        Some(B_TXT_BYTES)
    );
    assert_eq!(
        first.bytes(&catalogue_path("config/empty")),
        Some(b"".as_slice())
    );

    assert_eq!(
        first.manifest().content(),
        second.manifest().content(),
        "a repeated scan is identical"
    );
    assert_eq!(first.total_bytes(), second.total_bytes());
}

/// Hidden entries never enter the catalogue, and none of them is retained.
#[test]
fn hidden_files_and_directories_never_enter_the_catalogue() {
    let fixture = Fixture::open();
    fixture.write("mods/alpha.jar", ALPHA_BYTES);
    fixture.write("mods/.hidden.jar", b"hidden-by-file-name");
    fixture.write(".hiddendir/inside.jar", b"hidden-by-directory");
    fixture.write(".hiddendir/nested/deep.jar", b"hidden-by-depth");
    fixture.write("config/.secret", b"hidden-config");
    let preparation = preparation(&fixture);

    let snapshot = preparation
        .snapshot_with_disposition(
            crate::config::UnresolvedDisposition::EmbedPublic,
            metadata(),
            &empty_registry(),
        )
        .expect("hidden entries are excluded rather than fatal");
    assert_eq!(entries(&snapshot), ["mods/alpha.jar"]);
    for hidden in [
        "mods/.hidden.jar",
        ".hiddendir",
        ".hiddendir/inside.jar",
        "config/.secret",
    ] {
        assert!(
            snapshot.bytes(&catalogue_path(hidden)).is_none(),
            "{hidden}"
        );
    }
}

/// Secrets named by configuration are excluded, and so is the state directory the
/// canonical root contains.
#[test]
fn configured_and_state_paths_are_excluded_from_every_snapshot() {
    let fixture = Fixture::open();
    fixture.write("mods/alpha.jar", ALPHA_BYTES);
    let secret = fixture.mkdir("mods").join("server.properties");
    std::fs::write(&secret, b"op-password=let-me-in").expect("a secret that really exists");
    let config = fixture.game.join("wrapper-config.toml");
    std::fs::write(&config, b"token = \"secret\"\n").expect("a config that really exists");
    fixture.write("mods/.git/config", b"[core]\n");
    fixture.write("mods/.vesta/state.db", b"not a real database");
    let absent = fixture.game.join("mods/never-created.properties");

    let preparation = PackPreparation::open(
        &fixture.db,
        &fixture.game,
        &[&secret, &config, &absent],
        limits(1_024, 1_024, 32, 256),
    )
    .expect("a root with excluded secrets opens");

    let snapshot = preparation
        .snapshot_with_disposition(
            crate::config::UnresolvedDisposition::EmbedPublic,
            metadata(),
            &empty_registry(),
        )
        .expect("exclusions are skipped rather than fatal");
    assert_eq!(entries(&snapshot), ["mods/alpha.jar"]);
    for name in [
        "mods/host-secret.toml",
        "wrapper-config.toml",
        "mods/.git/config",
        "mods/.vesta/state.db",
        "mods/never-created.properties",
    ] {
        assert!(snapshot.bytes(&catalogue_path(name)).is_none(), "{name}");
    }
    assert_eq!(
        std::fs::read(&secret).expect("the secret is left untouched"),
        b"op-password=let-me-in"
    );
}

/// A root the state directory contains is refused outright rather than scanned.
#[test]
fn a_root_inside_the_state_directory_is_refused() {
    let fixture = Fixture::open();
    let nested = fixture.db_path().join("inside");
    std::fs::create_dir_all(&nested).expect("a directory inside the state root");
    std::fs::write(nested.join("alpha.jar"), ALPHA_BYTES).expect("fixture bytes");

    let error = PackPreparation::open(&fixture.db, &nested, &[], limits(64, 64, 4, 16))
        .err()
        .unwrap_or_else(|| panic!("a root inside the state directory must be refused"));
    assert_eq!(error, PackError::ProtectedPath);
    assert_eq!(error.code(), "pack_path_protected");
    assert_eq!(
        std::fs::read(nested.join("alpha.jar")).expect("bytes survive"),
        ALPHA_BYTES
    );
}

/// A hard link gives one file a second visible name, so preparation refuses rather than
/// cataloguing the same bytes twice under two entries.
#[test]
fn a_visible_hard_link_is_refused_and_the_file_is_left_intact() {
    let fixture = Fixture::open();
    fixture.write("mods/alpha.jar", ALPHA_BYTES);
    let alias = fixture.game.join("mods/alpha-link.jar");
    std::fs::hard_link(fixture.game.join("mods/alpha.jar"), &alias).expect("a fixture hard link");

    let preparation = preparation(&fixture);
    assert_pack_error(
        preparation.snapshot_with_disposition(
            crate::config::UnresolvedDisposition::EmbedPublic,
            metadata(),
            &empty_registry(),
        ),
        PackError::LinkedPath,
    );

    let selected =
        preparation.with_selected_directories(super::SelectedDirectories::new(&["mods"]).unwrap());
    assert_pack_error(
        selected.snapshot_with_disposition(
            crate::config::UnresolvedDisposition::EmbedPublic,
            metadata(),
            &empty_registry(),
        ),
        PackError::LinkedPath,
    );

    assert_eq!(
        std::fs::read(fixture.game.join("mods/alpha.jar")).expect("the original survives"),
        ALPHA_BYTES
    );
    assert_eq!(
        std::fs::read(&alias).expect("the link survives"),
        ALPHA_BYTES
    );
}

/// A junction exposes a directory the root does not contain. It is refused, and the
/// directory it points at is neither read nor changed.
#[cfg(windows)]
#[test]
fn a_visible_directory_junction_is_refused_and_the_target_is_left_intact() {
    const TARGET_BYTES: &[u8] = b"owned target bytes outside the scan root";
    let fixture = Fixture::open();
    fixture.write("mods/alpha.jar", ALPHA_BYTES);
    // The junction must point somewhere the scan root does not contain, or the refusal
    // below would prove nothing about crossing the root. The owned target is a sibling
    // of the game directory inside the fixture's temporary tree (game is `temp/game`,
    // so the sibling is `temp/<target>`), so the whole fixture still lives under the
    // TempDir the Fixture owns.
    let root = fixture.game.canonicalize().expect("an existing root");
    let target = root
        .parent()
        .expect("a game directory that is not the temporary root itself")
        .join("owned-junction-target");
    std::fs::create_dir(&target).expect("an owned target directory");
    let target_file = target.join("secret.jar");
    std::fs::write(&target_file, TARGET_BYTES).expect("owned bytes outside the root");

    // mklink is a cmd.exe internal, so it must run through cmd.exe. Both operands are
    // PathBufs built from components, so Windows separators are the path's own rather
    // than hand-written ones, and both stay separate argv entries for cmd to read
    // against the current directory: the link from inside the canonical root, the
    // target as a `..` path back out of it.
    let junction = PathBuf::from("mods").join("junction");
    let relative_target = PathBuf::from("..").join("owned-junction-target");
    // This fixture only creates a link to a directory this test owns. It creates no
    // file, opens no handle to the target and changes no byte; production code never
    // creates links and treats one only as a refusal.
    let made = std::process::Command::new("cmd.exe")
        .args(["/d", "/c", "mklink", "/J"])
        .arg(&junction)
        .arg(&relative_target)
        .current_dir(&root)
        .output()
        .expect("cmd.exe runs");
    // Judge the fixture by the native object it left behind, never by mklink's exit
    // code alone. `exists()` follows the link, so also require a real reparse point:
    // without one a plain directory would look like a passing fixture and the refusal
    // below would be testing the wrong thing. The followed path must also reach the
    // owned target, or nothing outside the root would have been crossed.
    let created = root.join(&junction);
    // symlink_metadata, not metadata: a junction's own attributes, never its target's.
    let reparse = std::fs::symlink_metadata(&created).is_ok_and(|metadata| {
        std::os::windows::fs::MetadataExt::file_attributes(&metadata) & 0x400 != 0
    });
    let followed = std::fs::canonicalize(&created).is_ok_and(|followed| {
        let owned = std::fs::canonicalize(&target).expect("the owned target exists");
        followed == owned
    });
    assert!(
        made.status.success(),
        "mklink must report success: {made:?}"
    );
    assert!(
        created.exists() && reparse && followed,
        "create the junction fixture that reaches the owned target: {made:?}"
    );

    let preparation = preparation(&fixture);
    assert_pack_error(
        preparation.snapshot_with_disposition(
            crate::config::UnresolvedDisposition::EmbedPublic,
            metadata(),
            &empty_registry(),
        ),
        PackError::LinkedPath,
    );

    let selected = preparation.with_selected_directories(
        super::SelectedDirectories::new(&["mods/junction/missing"]).unwrap(),
    );
    assert_pack_error(
        selected.snapshot_with_disposition(
            crate::config::UnresolvedDisposition::EmbedPublic,
            metadata(),
            &empty_registry(),
        ),
        PackError::LinkedPath,
    );

    assert_eq!(
        std::fs::read(fixture.game.join("mods/alpha.jar")).expect("the original survives"),
        ALPHA_BYTES
    );
    assert_eq!(
        std::fs::read(&target_file).expect("the owned target is untouched"),
        TARGET_BYTES
    );
}

/// Escaping spellings never become a relative path, so no archive can be pinned through
/// them. Archives are pinned as opaque bytes, whatever the case of the extension.
#[test]
fn traversal_is_refused_and_archives_are_opaque_regardless_of_case() {
    for escaping in [
        "../outside.jar",
        "mods/../../outside.jar",
        "/absolute.jar",
        "C:/win.jar",
    ] {
        let parsed = RelativePath::parse(escaping);
        assert_eq!(
            parsed.as_ref().err(),
            Some(&FileError::InvalidPath),
            "{escaping} must not become a relative path"
        );
    }

    let fixture = Fixture::open();
    fixture.write("archive/one.ZIP", ALPHA_BYTES);
    fixture.write("archive/two.MRPACK", B_TXT_BYTES);
    fixture.write("archive/three.zip", b"");
    fixture.write("archive/pack.tar.gz", b"not a supported archive");
    fixture.write("archive/plain", b"no extension at all");
    let preparation = preparation(&fixture);

    let upper_zip = preparation
        .archive(&relative("archive/one.ZIP"))
        .expect("an uppercase .ZIP is accepted");
    assert_eq!(upper_zip.bytes(), ALPHA_BYTES, "the bytes are not parsed");
    assert_eq!(upper_zip.description().sha256().to_hex(), ALPHA_HASH);
    assert_eq!(upper_zip.description().format(), ArchiveFormat::Zip);
    assert_eq!(upper_zip.description().size(), ALPHA_BYTES.len() as u64);

    let upper_mrpack = preparation
        .archive(&relative("archive/two.MRPACK"))
        .expect("an uppercase .MRPACK is accepted");
    assert_eq!(upper_mrpack.bytes(), B_TXT_BYTES);
    assert_eq!(upper_mrpack.description().sha256().to_hex(), B_TXT_HASH);
    assert_eq!(upper_mrpack.description().format(), ArchiveFormat::Mrpack);

    let empty = preparation
        .archive(&relative("archive/three.zip"))
        .expect("an empty archive is still pinned");
    assert_eq!(empty.bytes(), b"".as_slice());
    assert_eq!(empty.description().sha256().to_hex(), EMPTY_HASH);
    assert_eq!(empty.description().size(), 0);

    for refused in [
        "archive/pack.tar.gz",
        "archive/plain",
        "archive/one.ZIP.bak",
    ] {
        assert_pack_error(
            preparation.archive(&relative(refused)),
            PackError::InvalidArchive,
        );
    }
}

/// Prepared bytes are the artifact. Replacing or deleting the source afterwards must not
/// change what was already pinned.
#[test]
fn prepared_bytes_and_hashes_survive_source_overwrite_and_removal() {
    let fixture = Fixture::open();
    fixture.write("mods/alpha.jar", ALPHA_BYTES);
    fixture.write("archive/one.zip", ALPHA_BYTES);
    let preparation = preparation(&fixture);

    let snapshot = preparation
        .snapshot_with_disposition(
            crate::config::UnresolvedDisposition::EmbedPublic,
            metadata(),
            &empty_registry(),
        )
        .expect("the initial scan succeeds");
    let archive = preparation
        .archive(&relative("archive/one.zip"))
        .expect("the initial archive pins");

    fixture.write("mods/alpha.jar", b"replaced-content-that-is-longer");
    std::fs::remove_file(fixture.game.join("archive/one.zip")).expect("the archive source goes");

    assert_eq!(
        snapshot.bytes(&catalogue_path("mods/alpha.jar")),
        Some(ALPHA_BYTES)
    );
    assert_eq!(
        file_in(&snapshot, "mods/alpha.jar").sha256().to_hex(),
        ALPHA_HASH
    );
    assert_eq!(archive.bytes(), ALPHA_BYTES);
    assert_eq!(archive.description().sha256().to_hex(), ALPHA_HASH);

    let rescan = preparation
        .snapshot_with_disposition(
            crate::config::UnresolvedDisposition::EmbedPublic,
            metadata(),
            &empty_registry(),
        )
        .expect("a rescan after the change succeeds");
    assert_ne!(
        file_in(&rescan, "mods/alpha.jar").sha256().to_hex(),
        ALPHA_HASH,
        "a rescan reflects the new content"
    );
}

/// Each documented limit is inclusive, and one unit past it refuses the preparation.
#[test]
fn every_documented_work_limit_is_inclusive_and_refuses_one_more() {
    // File bytes: a body of exactly the limit is accepted, one byte more refuses.
    let fixture = Fixture::open();
    fixture.write("mods/alpha.jar", &[b'x'; 8]);
    let preparation = PackPreparation::open(&fixture.db, &fixture.game, &[], limits(8, 8, 4, 16))
        .expect("a root opens");
    assert_eq!(
        preparation
            .snapshot_with_disposition(
                crate::config::UnresolvedDisposition::EmbedPublic,
                metadata(),
                &empty_registry()
            )
            .expect("a body of exactly the file limit is accepted")
            .total_bytes(),
        8
    );
    fixture.write("mods/alpha.jar", &[b'x'; 9]);
    assert_pack_error(
        preparation.snapshot_with_disposition(
            crate::config::UnresolvedDisposition::EmbedPublic,
            metadata(),
            &empty_registry(),
        ),
        PackError::TooLarge,
    );

    // Total bytes: two files that each fit the file limit but jointly exceed the total.
    let fixture = Fixture::open();
    fixture.write("mods/a.jar", &[b'x'; 6]);
    fixture.write("config/b.txt", &[b'y'; 5]);
    let preparation = PackPreparation::open(&fixture.db, &fixture.game, &[], limits(6, 10, 4, 16))
        .expect("a root opens");
    assert_pack_error(
        preparation.snapshot_with_disposition(
            crate::config::UnresolvedDisposition::EmbedPublic,
            metadata(),
            &empty_registry(),
        ),
        PackError::TooLarge,
    );

    // File count: exactly the count is accepted, one more refuses.
    let fixture = Fixture::open();
    for index in 0..3 {
        fixture.write(&format!("mods/{index}.jar"), b"n");
    }
    let preparation = PackPreparation::open(&fixture.db, &fixture.game, &[], limits(4, 64, 3, 32))
        .expect("a root opens");
    assert_eq!(
        preparation
            .snapshot_with_disposition(
                crate::config::UnresolvedDisposition::EmbedPublic,
                metadata(),
                &empty_registry()
            )
            .expect("exactly the file count is accepted")
            .total_bytes(),
        3
    );
    fixture.write("mods/extra.jar", b"n");
    assert_pack_error(
        preparation.snapshot_with_disposition(
            crate::config::UnresolvedDisposition::EmbedPublic,
            metadata(),
            &empty_registry(),
        ),
        PackError::TooLarge,
    );

    // Scan budget: every directory entry the tree offers is charged, including the two
    // visible directory names, the two visible files and the four hidden files. That is
    // eight entries, so a budget of eight accepts this tree and seven refuses.
    let fixture = Fixture::open();
    fixture.write("mods/alpha.jar", b"n");
    fixture.write("config/b.txt", b"n");
    for index in 0..4 {
        fixture.write(&format!("mods/.hidden{index}"), b"n");
    }
    let exact = PackPreparation::open(&fixture.db, &fixture.game, &[], limits(4, 64, 4, 8))
        .expect("a root opens");
    let accepted = exact
        .snapshot_with_disposition(
            crate::config::UnresolvedDisposition::EmbedPublic,
            metadata(),
            &empty_registry(),
        )
        .expect("a budget of exactly the entry count is accepted");
    assert_eq!(entries(&accepted), ["config/b.txt", "mods/alpha.jar"]);
    assert_eq!(accepted.total_bytes(), 2);

    let short = PackPreparation::open(&fixture.db, &fixture.game, &[], limits(4, 64, 4, 7))
        .expect("a root opens");
    assert_pack_error(
        short.snapshot_with_disposition(
            crate::config::UnresolvedDisposition::EmbedPublic,
            metadata(),
            &empty_registry(),
        ),
        PackError::TooLarge,
    );

    // The smallest legal budget is one entry, which an empty directory never uses.
    let empty = Fixture::open();
    let zero = PackPreparation::open(&empty.db, &empty.game, &[], limits(4, 64, 4, 1))
        .expect("a root opens");
    let untouched = zero
        .snapshot_with_disposition(
            crate::config::UnresolvedDisposition::EmbedPublic,
            metadata(),
            &empty_registry(),
        )
        .expect("an empty tree has nothing to read");
    assert_eq!(untouched.total_bytes(), 0);
    assert!(untouched.bytes(&catalogue_path("mods/alpha.jar")).is_none());

    // The documented maxima are the values the limits are checked against.
    assert_eq!(MAX_PINNED_FILE_BYTES, 16_777_216);
    assert_eq!(MAX_PINNED_BYTES, 67_108_864);
    assert_eq!(MAX_SNAPSHOT_FILES, 4_096);
    assert_eq!(MAX_SNAPSHOT_SCAN, 16_384);
    assert_pack_error(
        SnapshotLimits::new(0, MAX_PINNED_BYTES, 1, 1),
        PackError::InvalidLimit,
    );
    assert_pack_error(
        SnapshotLimits::new(MAX_PINNED_FILE_BYTES + 1, MAX_PINNED_BYTES, 1, 1),
        PackError::InvalidLimit,
    );
    assert_pack_error(SnapshotLimits::new(16, 8, 1, 1), PackError::InvalidLimit);
}

/// A malformed registry is refused outright. Treating it as empty would let a known file
/// fall back to embedding its bytes, which is exactly what must not happen.
#[test]
fn a_malformed_source_registry_is_refused_rather_than_treated_as_empty() {
    let malformed: &[&[u8]] = &[
        b"",
        b"{",
        b"[]",
        b"null",
        br#"{"schemaVersion":1}"#,
        br#"{"schemaVersion":99,"files":[]}"#,
        br#"{"schemaVersion":1,"files":{}}"#,
        br#"{"schemaVersion":1,"files":[],"extra":true}"#,
        br#"{"schemaVersion":1,"files":[{"path":"mods/a.jar","sha256":"nope","platform":"modrinth","projectId":"A","versionId":"B"}]}"#,
        br#"{"schemaVersion":1,"files":[{"path":"../escape","sha256":"0000000000000000000000000000000000000000000000000000000000000000","platform":"modrinth","projectId":"A","versionId":"B"}]}"#,
        br#"{"schemaVersion":1,"files":[{"path":"mods/a.jar","sha256":"0000000000000000000000000000000000000000000000000000000000000000","platform":"http","projectId":"A","versionId":"B"}]}"#,
    ];
    for bytes in malformed {
        assert!(
            SourceRegistry::decode(bytes).is_err(),
            "{:?} must not decode as a registry",
            String::from_utf8_lossy(bytes)
        );
    }
}

/// A known file whose content no longer matches its registered digest refuses, instead of
/// silently embedding bytes under a stale provider description.
#[test]
fn a_stale_registered_hash_refuses_instead_of_silently_embedding() {
    let fixture = Fixture::open();
    // The file holds STALE_BYTES, but the registry pins the *known* content for this
    // path: the on-disk bytes are stale relative to what the registry vouches for.
    fixture.write("mods/stale.jar", STALE_BYTES);
    let registry = SourceRegistry::new(vec![
        KnownSource::new(
            "mods/stale.jar",
            KNOWN_HASH,
            FileSource::platform(Platform::Modrinth, "Abcd1234", "Efgh5678", None)
                .expect("a valid platform source"),
        )
        .expect("a valid known source"),
    ])
    .expect("a valid registry");

    assert_catalogue_error(
        preparation(&fixture).snapshot_with_disposition(
            crate::config::UnresolvedDisposition::EmbedPublic,
            metadata(),
            &registry,
        ),
        CatalogueError::InvalidHash,
    );
}

/// An exact path and content match keeps its platform description; an unmatched file is
/// embedded, with no invented provider identity.
#[test]
fn an_exact_platform_match_is_kept_and_an_unmatched_file_is_embedded() {
    let fixture = Fixture::open();
    fixture.write("mods/known.jar", KNOWN_BYTES);
    fixture.write("mods/unknown.jar", ALPHA_BYTES);
    let registry = SourceRegistry::new(vec![
        KnownSource::new(
            "mods/known.jar",
            KNOWN_HASH,
            FileSource::platform(
                Platform::Modrinth,
                "Abcd1234",
                "Efgh5678",
                Some("https://cdn.modrinth.com/data/Abcd1234/versions/Efgh5678/known.jar"),
            )
            .expect("a valid platform source"),
        )
        .expect("a valid known source"),
    ])
    .expect("a valid registry");

    let snapshot = preparation(&fixture)
        .snapshot_with_disposition(
            crate::config::UnresolvedDisposition::EmbedPublic,
            metadata(),
            &registry,
        )
        .expect("a mixed snapshot succeeds");
    let known = file_in(&snapshot, "mods/known.jar");
    let unknown = file_in(&snapshot, "mods/unknown.jar");

    assert_eq!(known.sha256().to_hex(), KNOWN_HASH);
    assert_eq!(known.source().kind(), SourceKind::Platform);
    assert_eq!(known.source().platform_name(), Some(Platform::Modrinth));
    assert_eq!(known.source().project_id(), Some("Abcd1234"));
    assert_eq!(known.source().version_id(), Some("Efgh5678"));

    assert_eq!(unknown.source().kind(), SourceKind::Embedded);
    assert_eq!(unknown.source().platform_name(), None);
    assert_eq!(unknown.source().project_id(), None);
    assert_eq!(unknown.source().url(), None);
    assert_eq!(
        snapshot.bytes(&catalogue_path("mods/unknown.jar")),
        Some(ALPHA_BYTES)
    );
}

/// A registered path that differs only by ASCII case must not match, and must not produce
/// a second entry.
#[test]
fn a_case_variant_of_a_registered_path_does_not_match() {
    let fixture = Fixture::open();
    fixture.write("mods/Known.jar", KNOWN_BYTES);
    let registry = SourceRegistry::new(vec![
        KnownSource::new(
            "mods/known.jar",
            KNOWN_HASH,
            FileSource::platform(Platform::Curseforge, "123456", "1234567", None)
                .expect("a valid platform source"),
        )
        .expect("a valid known source"),
    ])
    .expect("a valid registry");

    assert_catalogue_error(
        preparation(&fixture).snapshot_with_disposition(
            crate::config::UnresolvedDisposition::EmbedPublic,
            metadata(),
            &registry,
        ),
        CatalogueError::PathCollision,
    );
}

/// Debug output and errors describe structure and codes only. No path, byte, digest or
/// secret from the tree may appear.
#[test]
fn debug_and_errors_leak_no_paths_bytes_or_digests() {
    let fixture = Fixture::open();
    fixture.write("mods/alpha.jar", ALPHA_BYTES);
    fixture.write("config/b.txt", B_TXT_BYTES);
    let preparation = preparation(&fixture);

    let rendered = format!("{preparation:?}");
    assert_eq!(rendered, "PackPreparation[<redacted>]");
    for secret in [
        "alpha",
        "b.txt",
        ALPHA_HASH,
        fixture.game.to_str().unwrap_or(""),
    ] {
        assert!(
            !rendered.contains(secret),
            "{secret} leaked into {rendered}"
        );
    }

    let snapshot = preparation
        .snapshot_with_disposition(
            crate::config::UnresolvedDisposition::EmbedPublic,
            metadata(),
            &empty_registry(),
        )
        .expect("a snapshot succeeds");
    let snapshot_debug = format!("{snapshot:?}");
    for secret in ["alpha", "b.txt", ALPHA_HASH, B_TXT_HASH] {
        assert!(
            !snapshot_debug.contains(secret),
            "{secret} leaked into {snapshot_debug}"
        );
    }

    let archive = preparation
        .archive(&relative("config/b.txt"))
        .expect_err("a .txt is not an archive");
    assert_eq!(archive, PackError::InvalidArchive);
    assert_eq!(archive.to_string(), "pack_archive_invalid");
    assert!(std::error::Error::source(&archive).is_none());

    let path_error = PackPreparation::open(&fixture.db, &fixture.state, &[], limits(64, 64, 4, 16))
        .err()
        .unwrap_or_else(|| panic!("the state root itself must be refused as a pack root"));
    assert_eq!(path_error.to_string(), "pack_path_protected");
    assert!(
        !path_error.to_string().contains("private-state"),
        "{path_error}"
    );

    for error in [
        PackError::InvalidLimit,
        PackError::InvalidPath,
        PackError::ProtectedPath,
        PackError::LinkedPath,
        PackError::Unavailable,
        PackError::TooLarge,
        PackError::InvalidArchive,
    ] {
        assert_eq!(error.to_string(), error.code());
        assert!(error.code().starts_with("pack_"), "{}", error.code());
    }
}

#[test]
fn selected_directories_keep_root_names_and_report_only_absent_selections() {
    use super::SelectedDirectories;
    use crate::config::UnresolvedDisposition;
    let f = Fixture::open();
    f.write("mods/alpha.jar", ALPHA_BYTES);
    f.write("config/deep/b.txt", B_TXT_BYTES);
    f.write("other/not-selected", b"outside");
    f.write("config/.hidden/secret", b"hidden");
    f.mkdir("empty");
    let prepare = preparation(&f)
        .with_unresolved_disposition(UnresolvedDisposition::EmbedPublic)
        .with_selected_directories(
            SelectedDirectories::new(&["missing/deep", "mods", "empty", "config/deep", ".hidden"])
                .unwrap(),
        );
    let snapshot = prepare.snapshot(metadata(), &empty_registry()).unwrap();
    assert_eq!(entries(&snapshot), ["config/deep/b.txt", "mods/alpha.jar"]);
    assert_eq!(
        snapshot
            .missing_directories()
            .iter()
            .map(RelativePath::as_str)
            .collect::<Vec<_>>(),
        ["missing/deep"]
    );
    assert_eq!(
        snapshot.bytes(&CataloguePath::parse("mods/alpha.jar").unwrap()),
        Some(ALPHA_BYTES)
    );
    assert_eq!(
        snapshot.bytes(&CataloguePath::parse("config/deep/b.txt").unwrap()),
        Some(B_TXT_BYTES)
    );
    assert_eq!(
        snapshot.total_bytes(),
        ALPHA_BYTES.len() + B_TXT_BYTES.len()
    );
}

#[test]
fn directory_selections_reject_duplicate_overlap_and_directory_case_aliases() {
    use super::SelectedDirectories;
    for paths in [
        vec!["mods", "mods"],
        vec!["mods", "MODS"],
        vec!["mods", "mods/sub"],
        vec!["mods/sub", "mods"],
        vec!["mods/a", "Mods/b"],
        vec!["a/B/c", "a/b/d"],
        vec!["../mods"],
        vec!["/mods"],
        vec!["mods//sub"],
        vec![r"mods\sub"],
        vec!["mods/CON"],
        vec![".vesta"],
        vec![""],
        vec![],
    ] {
        assert!(SelectedDirectories::new(&paths).is_err(), "{paths:?}");
    }
    assert!(SelectedDirectories::new(&["mods/a", "mods/b", "mods-other"]).is_ok());
    let paths = (0..=super::MAX_SELECTED_DIRECTORIES)
        .map(|n| format!("dir{n}"))
        .collect::<Vec<_>>();
    assert!(SelectedDirectories::new(&paths).is_err());
}

#[test]
fn selected_scans_keep_exclusions_dispositions_and_global_budgets() {
    use super::SelectedDirectories;
    use crate::config::UnresolvedDisposition;
    let f = Fixture::open();
    f.write("config/public.txt", B_TXT_BYTES);
    f.write("config/protected.txt", b"secret");
    f.write("config/wrapper-config.toml", b"reserved secret");
    f.write("config/.hidden", b"hidden secret");
    let secret = f.game.join("config/protected.txt");
    let p = PackPreparation::open(&f.db, &f.game, &[&secret], limits(1024, 1024, 32, 256))
        .unwrap()
        .with_selected_directories(SelectedDirectories::new(&["config", "not-present"]).unwrap());
    assert!(matches!(
        p.snapshot(metadata(), &empty_registry()),
        Err(PackError::UnresolvedFile)
    ));
    let public = p
        .snapshot_with_disposition(
            UnresolvedDisposition::EmbedPublic,
            metadata(),
            &empty_registry(),
        )
        .unwrap();
    assert_eq!(entries(&public), ["config/public.txt"]);
    let private = p
        .snapshot_with_disposition(
            UnresolvedDisposition::ExcludePrivate,
            metadata(),
            &empty_registry(),
        )
        .unwrap();
    assert!(entries(&private).is_empty());
    assert_eq!(private.private_files().len(), 1);
    assert_eq!(
        private.private_files()[0].path().as_str(),
        "config/public.txt"
    );
    let stale = SourceRegistry::new(vec![
        KnownSource::new(
            "config/public.txt",
            EMPTY_HASH,
            FileSource::platform(Platform::Modrinth, "p", "v", None).unwrap(),
        )
        .unwrap(),
    ])
    .unwrap();
    assert!(
        p.snapshot_with_disposition(UnresolvedDisposition::ExcludePrivate, metadata(), &stale)
            .is_err()
    );
    let selections = SelectedDirectories::new(&["config", "absent"]).unwrap();
    let bounded = PackPreparation::open(&f.db, &f.game, &[&secret], limits(1024, 1024, 32, 2))
        .unwrap()
        .with_selected_directories(selections);
    assert!(matches!(
        bounded.snapshot_with_disposition(
            UnresolvedDisposition::EmbedPublic,
            metadata(),
            &empty_registry()
        ),
        Err(PackError::TooLarge)
    ));
    let direct = PackPreparation::open(
        &f.db,
        &f.game,
        &[&f.game.join("private")],
        limits(1024, 1024, 32, 256),
    )
    .unwrap()
    .with_selected_directories(SelectedDirectories::new(&["private/missing"]).unwrap());
    let excluded = direct.snapshot(metadata(), &empty_registry()).unwrap();
    assert!(excluded.missing_directories().is_empty());
    assert!(entries(&excluded).is_empty());
}

#[test]
fn selected_directories_refuse_regular_files_and_missing_root_is_not_a_skipped_selection() {
    use super::SelectedDirectories;
    let f = Fixture::open();
    f.write("plain", b"file");
    let p =
        preparation(&f).with_selected_directories(SelectedDirectories::new(&["plain"]).unwrap());
    assert!(matches!(
        p.snapshot(metadata(), &empty_registry()),
        Err(PackError::Unavailable)
    ));
    let p =
        preparation(&f).with_selected_directories(SelectedDirectories::new(&["absent"]).unwrap());
    std::fs::rename(&f.game, f.game.with_extension("moved")).unwrap();
    assert!(matches!(
        p.snapshot(metadata(), &empty_registry()),
        Err(PackError::Unavailable)
    ));
}

#[test]
fn selections_share_file_and_byte_budgets_including_private_files() {
    use super::SelectedDirectories;
    use crate::config::UnresolvedDisposition;
    let f = Fixture::open();
    f.write("a/one", b"123");
    f.write("b/two", b"456");
    for (budget, disposition) in [
        (limits(3, 6, 1, 256), UnresolvedDisposition::EmbedPublic),
        (limits(3, 5, 2, 256), UnresolvedDisposition::EmbedPublic),
        (limits(3, 5, 2, 256), UnresolvedDisposition::ExcludePrivate),
    ] {
        let p = PackPreparation::open(&f.db, &f.game, &[], budget)
            .unwrap()
            .with_selected_directories(SelectedDirectories::new(&["a", "b"]).unwrap());
        assert_pack_error(
            p.snapshot_with_disposition(disposition, metadata(), &empty_registry()),
            PackError::TooLarge,
        );
    }
    let p = PackPreparation::open(&f.db, &f.game, &[], limits(3, 6, 2, 256))
        .unwrap()
        .with_selected_directories(SelectedDirectories::new(&["a", "b"]).unwrap());
    assert_eq!(
        p.snapshot_with_disposition(
            UnresolvedDisposition::EmbedPublic,
            metadata(),
            &empty_registry()
        )
        .unwrap()
        .total_bytes(),
        6
    );
}
