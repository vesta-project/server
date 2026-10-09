//! Real protected-state publication fixtures. Owned temporary directories only; no
//! listener, route, provider download, archive extraction or game execution.
use super::*;
use crate::packs::{PackPreparation, SnapshotLimits, VestaBuildLimits, build_vesta};
use std::{cell::Cell, path::PathBuf, sync::Arc};
use tempfile::TempDir;
use vesta_protocol::pack::{catalogue::ManifestMetadata, sources::SourceRegistry};
use vesta_state::{Clock, StateDirectory};

thread_local! {
    static FAULT: Cell<Option<Fault>> = const { Cell::new(None) };
}
pub(super) fn fault(point: Fault) -> Result<(), PackError> {
    if FAULT.with(|fault| fault.get()) == Some(point) {
        FAULT.with(|fault| fault.set(None));
        return Err(PackError::Unavailable);
    }
    Ok(())
}
fn inject(point: Fault) {
    FAULT.with(|fault| fault.set(Some(point)));
}

const A: &[u8] = b"first owned artifact";
const B: &[u8] = b"second owned artifact";

struct FixedClock;
impl Clock for FixedClock {
    fn millis(&self) -> i64 {
        1_791_115_200_000
    }
}
struct Fixture {
    temp: TempDir,
    game: PathBuf,
    state: PathBuf,
    db: StateDatabase,
}
impl Fixture {
    fn open() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let game = temp.path().join("game");
        std::fs::create_dir(&game).unwrap();
        let state = temp.path().join("private-state");
        let directory = StateDirectory::new(&state, Some(&game)).unwrap();
        let db = StateDatabase::open(directory, Arc::new(FixedClock)).unwrap();
        Self {
            temp,
            game,
            state,
            db,
        }
    }
    fn artifacts_dir(&self) -> PathBuf {
        // The canonical state root, as the database observed it.
        self.db.directory().path().join("artifacts")
    }
    fn names(&self) -> Vec<String> {
        let mut names: Vec<_> = std::fs::read_dir(self.artifacts_dir())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        names
    }
}

fn digest(bytes: &[u8]) -> Sha256 {
    Sha256::parse(&hex::encode(DigestSha256::digest(bytes))).unwrap()
}
fn published_name(bytes: &[u8], extension: &str) -> String {
    format!("{}.{extension}", digest(bytes).to_hex())
}

#[track_caller]
fn assert_error<T: fmt::Debug>(result: Result<T, PackError>, expected: PackError) {
    match result {
        Err(actual) => assert_eq!(actual, expected),
        Ok(value) => panic!("expected {expected:?}, produced {value:?}"),
    }
}

#[test]
fn publication_is_content_addressed_verified_and_idempotent() {
    let fixture = Fixture::open();
    let store = PackArtifacts::open(&fixture.db).unwrap();
    let artifact = store.publish(ArtifactFormat::Zip, A, &digest(A)).unwrap();
    assert_eq!(artifact.format(), ArtifactFormat::Zip);
    assert_eq!(artifact.size(), A.len() as u64);
    assert_eq!(artifact.sha256(), &digest(A));
    assert_eq!(fixture.names(), [published_name(A, "zip")]);
    assert_eq!(
        std::fs::read(fixture.artifacts_dir().join(published_name(A, "zip"))).unwrap(),
        A
    );
    let loaded = store.load(&artifact, MAX_ARTIFACT_BYTES).unwrap();
    assert_eq!(loaded.bytes(), A);
    assert_eq!(loaded.artifact(), &artifact);

    // Repeat publication of identical content neither rewrites nor duplicates.
    let again = store.publish(ArtifactFormat::Zip, A, &digest(A)).unwrap();
    assert_eq!(again, artifact);
    assert_eq!(fixture.names(), [published_name(A, "zip")]);

    // Format is part of the identity; the same bytes may be published as another kind.
    let mrpack = store
        .publish(ArtifactFormat::Mrpack, A, &digest(A))
        .unwrap();
    assert_ne!(mrpack, artifact);
    assert_eq!(
        fixture.names(),
        [published_name(A, "mrpack"), published_name(A, "zip")]
    );
    // A fresh handle over the same state observes the same publications.
    let reopened = PackArtifacts::open(&fixture.db).unwrap();
    assert_eq!(reopened.load(&mrpack, A.len()).unwrap().bytes(), A);
}

#[test]
fn later_publications_never_replace_or_remove_earlier_artifacts() {
    let fixture = Fixture::open();
    let store = PackArtifacts::open(&fixture.db).unwrap();
    let first = store.publish(ArtifactFormat::Vesta, A, &digest(A)).unwrap();
    let second = store.publish(ArtifactFormat::Vesta, B, &digest(B)).unwrap();
    assert_eq!(store.load(&first, A.len()).unwrap().bytes(), A);
    assert_eq!(store.load(&second, B.len()).unwrap().bytes(), B);
    assert_eq!(
        fixture.names().len(),
        2,
        "no staged residue beside two published names"
    );
}

#[test]
fn bytes_are_never_published_under_a_digest_they_do_not_have() {
    let fixture = Fixture::open();
    let store = PackArtifacts::open(&fixture.db).unwrap();
    assert_error(
        store.publish(ArtifactFormat::Zip, A, &digest(B)),
        PackError::Conflict,
    );
    assert!(fixture.names().is_empty());
}

#[test]
fn an_existing_conflicting_name_is_refused_and_left_byte_for_byte_intact() {
    let fixture = Fixture::open();
    let store = PackArtifacts::open(&fixture.db).unwrap();
    let occupied = fixture.artifacts_dir().join(published_name(A, "zip"));
    // Same length, different content: neither size nor name may be trusted.
    let impostor = vec![b'x'; A.len()];
    std::fs::write(&occupied, &impostor).unwrap();
    assert_error(
        store.publish(ArtifactFormat::Zip, A, &digest(A)),
        PackError::Conflict,
    );
    assert_eq!(std::fs::read(&occupied).unwrap(), impostor);
    assert_eq!(fixture.names(), [published_name(A, "zip")]);
}

#[test]
fn a_failure_before_linking_publishes_nothing_and_leaves_no_stage() {
    let fixture = Fixture::open();
    let store = PackArtifacts::open(&fixture.db).unwrap();
    let earlier = store.publish(ArtifactFormat::Zip, A, &digest(A)).unwrap();
    inject(Fault::BeforeLink);
    assert_error(
        store.publish(ArtifactFormat::Zip, B, &digest(B)),
        PackError::Unavailable,
    );
    assert_eq!(fixture.names(), [published_name(A, "zip")]);
    assert_eq!(store.load(&earlier, A.len()).unwrap().bytes(), A);
    let retried = store.publish(ArtifactFormat::Zip, B, &digest(B)).unwrap();
    assert_eq!(store.load(&retried, B.len()).unwrap().bytes(), B);
}

#[test]
fn a_failure_after_linking_still_retires_the_stage_and_a_retry_is_idempotent() {
    let fixture = Fixture::open();
    let store = PackArtifacts::open(&fixture.db).unwrap();
    inject(Fault::AfterLink);
    assert_error(
        store.publish(ArtifactFormat::Vesta, A, &digest(A)),
        PackError::Unavailable,
    );
    // The staged second name is removed on drop, so the published file is single-link.
    assert_eq!(fixture.names(), [published_name(A, "vestapack")]);
    let artifact = store.publish(ArtifactFormat::Vesta, A, &digest(A)).unwrap();
    assert_eq!(store.load(&artifact, A.len()).unwrap().bytes(), A);
}

#[test]
fn modified_truncated_or_removed_published_bytes_are_never_served() {
    let fixture = Fixture::open();
    let store = PackArtifacts::open(&fixture.db).unwrap();
    let artifact = store.publish(ArtifactFormat::Zip, A, &digest(A)).unwrap();
    let path = fixture.artifacts_dir().join(published_name(A, "zip"));

    std::fs::write(&path, vec![b'y'; A.len()]).unwrap();
    assert_error(store.load(&artifact, A.len()), PackError::Conflict);
    std::fs::write(&path, &A[..A.len() - 1]).unwrap();
    assert_error(store.load(&artifact, A.len()), PackError::Conflict);
    std::fs::write(&path, [A, b"!"].concat()).unwrap();
    assert_error(store.load(&artifact, A.len() + 1), PackError::Conflict);
    std::fs::remove_file(&path).unwrap();
    assert_error(store.load(&artifact, A.len()), PackError::NotPublished);
}

#[test]
fn an_extra_hard_link_to_a_published_name_refuses_the_load() {
    let fixture = Fixture::open();
    let store = PackArtifacts::open(&fixture.db).unwrap();
    let artifact = store.publish(ArtifactFormat::Zip, A, &digest(A)).unwrap();
    let alias = fixture.temp.path().join("outside-alias");
    std::fs::hard_link(
        fixture.artifacts_dir().join(published_name(A, "zip")),
        &alias,
    )
    .unwrap();
    assert_error(store.load(&artifact, A.len()), PackError::LinkedPath);
    std::fs::remove_file(&alias).unwrap();
    assert_eq!(store.load(&artifact, A.len()).unwrap().bytes(), A);
}

#[cfg(windows)]
#[test]
fn an_artifact_directory_junction_is_refused_and_its_target_is_untouched() {
    let fixture = Fixture::open();
    let outside = fixture.temp.path().join("owned-outside-target");
    std::fs::create_dir(&outside).unwrap();
    std::fs::write(outside.join("kept.txt"), b"kept").unwrap();
    let junction = fixture.artifacts_dir();
    let made = std::process::Command::new("cmd.exe")
        .args(["/d", "/c", "mklink", "/J"])
        .arg(&junction)
        .arg(&outside)
        .output()
        .expect("cmd.exe runs");
    assert!(made.status.success(), "mklink must succeed: {made:?}");
    let reparse = std::fs::symlink_metadata(&junction).is_ok_and(|metadata| {
        std::os::windows::fs::MetadataExt::file_attributes(&metadata) & 0x400 != 0
    });
    assert!(reparse, "the fixture must be an actual reparse point");
    assert_error(PackArtifacts::open(&fixture.db), PackError::LinkedPath);
    let entries: Vec<_> = std::fs::read_dir(&outside).unwrap().collect();
    assert_eq!(entries.len(), 1);
    assert_eq!(std::fs::read(outside.join("kept.txt")).unwrap(), b"kept");
}

#[cfg(windows)]
#[test]
fn a_junction_substituted_after_opening_refuses_publication_and_loads() {
    let fixture = Fixture::open();
    let store = PackArtifacts::open(&fixture.db).unwrap();
    let artifact = store.publish(ArtifactFormat::Zip, A, &digest(A)).unwrap();
    let moved = fixture.temp.path().join("moved-artifacts");
    std::fs::rename(fixture.artifacts_dir(), &moved).unwrap();
    let made = std::process::Command::new("cmd.exe")
        .args(["/d", "/c", "mklink", "/J"])
        .arg(fixture.artifacts_dir())
        .arg(&moved)
        .output()
        .expect("cmd.exe runs");
    assert!(made.status.success(), "mklink must succeed: {made:?}");
    assert_error(store.load(&artifact, A.len()), PackError::LinkedPath);
    assert_error(
        store.publish(ArtifactFormat::Zip, B, &digest(B)),
        PackError::LinkedPath,
    );
    let mut moved_names: Vec<_> = std::fs::read_dir(&moved)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    moved_names.sort();
    assert_eq!(moved_names, [published_name(A, "zip")]);
}

#[test]
fn the_entry_bound_is_inclusive_and_counts_stale_entries() {
    let fixture = Fixture::open();
    let store = PackArtifacts::open(&fixture.db).unwrap();
    for index in 0..MAX_ARTIFACT_ENTRIES - 1 {
        std::fs::write(fixture.artifacts_dir().join(format!("filler-{index}")), b"").unwrap();
    }
    let last = store.publish(ArtifactFormat::Zip, A, &digest(A)).unwrap();
    assert_eq!(fixture.names().len(), MAX_ARTIFACT_ENTRIES);
    assert_error(
        store.publish(ArtifactFormat::Zip, B, &digest(B)),
        PackError::TooLarge,
    );
    // Identical republication needs no new entry and remains available at the bound.
    assert_eq!(
        store.publish(ArtifactFormat::Zip, A, &digest(A)).unwrap(),
        last
    );
    assert_eq!(fixture.names().len(), MAX_ARTIFACT_ENTRIES);
}

#[test]
fn load_limits_are_checked_before_any_read() {
    let fixture = Fixture::open();
    let store = PackArtifacts::open(&fixture.db).unwrap();
    let artifact = store.publish(ArtifactFormat::Zip, A, &digest(A)).unwrap();
    assert_error(store.load(&artifact, 0), PackError::InvalidLimit);
    assert_error(
        store.load(&artifact, MAX_ARTIFACT_BYTES + 1),
        PackError::InvalidLimit,
    );
    assert_error(store.load(&artifact, A.len() - 1), PackError::TooLarge);
    assert_eq!(store.load(&artifact, A.len()).unwrap().bytes(), A);
}

#[test]
fn prepared_archives_and_built_packs_publish_their_own_verified_bytes() {
    let fixture = Fixture::open();
    std::fs::create_dir_all(fixture.game.join("config")).unwrap();
    std::fs::write(fixture.game.join("config/options.txt"), b"embedded").unwrap();
    std::fs::create_dir_all(fixture.game.join("archive")).unwrap();
    std::fs::write(fixture.game.join("archive/pack.mrpack"), B).unwrap();
    let preparation = PackPreparation::open(
        &fixture.db,
        &fixture.game,
        &[],
        SnapshotLimits::new(1_024, 1_024, 8, 64).unwrap(),
    )
    .unwrap();
    let archive = preparation
        .archive(&crate::files::RelativePath::parse("archive/pack.mrpack").unwrap())
        .unwrap();
    let host = crate::config::HostConfiguration::parse_toml(
        br#"schema-version = 1
[paths]
state = 'private-state'
game = 'game'
scan-roots = ['game/config']
launch-definition = 'private/launch.toml'
[pack]
name = 'Published'
version = '1.0.0'
revision = 2
minimum-revision = 1
minecraft-version = '1.21.1'
[pack-access]
mode = 'minecraft'
[listener]
mode = 'disabled'
"#,
        fixture.temp.path(),
        std::path::Path::new("host.toml"),
    )
    .unwrap();
    let pack = host.pack();
    let snapshot = preparation
        .snapshot(
            ManifestMetadata::new(
                pack.name(),
                pack.version(),
                pack.revision(),
                pack.minimum_revision(),
            )
            .unwrap(),
            &SourceRegistry::new(Vec::new()).unwrap(),
        )
        .unwrap();
    let built = build_vesta(&snapshot, pack, VestaBuildLimits::production()).unwrap();

    let store = PackArtifacts::open(&fixture.db).unwrap();
    let published_archive = store.publish_archive(&archive).unwrap();
    let published_pack = store.publish_vesta(&built).unwrap();
    assert_eq!(published_archive.format(), ArtifactFormat::Mrpack);
    assert_eq!(published_pack.format(), ArtifactFormat::Vesta);
    assert_eq!(published_pack.sha256(), built.sha256());

    // Later source replacement cannot change what was already published.
    std::fs::write(fixture.game.join("archive/pack.mrpack"), A).unwrap();
    std::fs::write(fixture.game.join("config/options.txt"), b"replaced").unwrap();
    assert_eq!(
        store
            .load(&published_archive, MAX_ARTIFACT_BYTES)
            .unwrap()
            .bytes(),
        B
    );
    assert_eq!(
        store
            .load(&published_pack, MAX_ARTIFACT_BYTES)
            .unwrap()
            .bytes(),
        built.bytes()
    );
    // Publication writes only protected state, never the game tree.
    assert!(!fixture.game.join(".vesta").exists());
    assert!(fixture.state.exists());
}

#[test]
fn debug_output_redacts_digests_and_bytes() {
    let fixture = Fixture::open();
    let store = PackArtifacts::open(&fixture.db).unwrap();
    let artifact = store.publish(ArtifactFormat::Zip, A, &digest(A)).unwrap();
    let loaded = store.load(&artifact, A.len()).unwrap();
    let rendered = format!("{store:?} {artifact:?} {loaded:?}");
    for secret in [
        digest(A).to_hex(),
        String::from_utf8(A.to_vec()).unwrap(),
        fixture.state.display().to_string(),
    ] {
        assert!(!rendered.contains(&secret), "{rendered}");
    }
    for error in [PackError::Conflict, PackError::NotPublished] {
        assert!(error.code().starts_with("pack_artifact_"));
        assert_eq!(error.to_string(), error.code());
    }
}
