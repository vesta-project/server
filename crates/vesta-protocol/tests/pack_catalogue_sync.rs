//! Independent golden and adversarial assertions for the validated native API.
//! No Java process, filesystem inventory, HTTP client, or installation is involved.

use std::error::Error;
use vesta_protocol::pack::catalogue::{
    ArchiveFormat, ArchivePack, CatalogueError, FileSource, MAX_COMPONENT_BYTES, MAX_FILE_SIZE,
    MAX_FILES, MAX_JSON_BYTES, MAX_PATH_BYTES, MAX_PATH_COMPONENTS, MAX_REVISION,
    MAX_SOURCE_ID_BYTES, MAX_SOURCE_URL_BYTES, MAX_TEXT_BYTES, MAX_TOTAL_SIZE, MAX_TOTAL_TEXT_BYTES, Manifest,
    ManifestMetadata, PackContent, PackFile, Platform, PlatformPack, RelativePath, Sha256,
    SourceKind,
};
use vesta_protocol::pack::sync::{
    InstalledFile, InstalledState, RevisionStatus, SyncError, SyncOperation, plan,
};

const A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const C: &str = "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";
const D: &str = "dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd";
const E: &str = "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee";
const F: &str = "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff";

// Full-width hashes are deliberate: short Java unit-test hash placeholders do not
// establish integrity and are not valid inputs to this native application boundary.
const GOLDEN_MANIFEST: &[u8] = br#"{
  "schemaVersion":1,"name":"Golden pack","version":"2.0.0",
  "revision":5,"minimumRevision":3,
  "content":{"mode":"custom","files":[
    {"path":"mods/new.jar","sha256":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","size":20,"source":{"type":"platform","platform":"modrinth","projectId":"project-1","versionId":"version_2","url":"https://example.invalid/new.jar"}},
    {"path":"mods/keep.jar","sha256":"AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA","sha1":"0123456789abcdef0123456789abcdef01234567","size":10,"source":{"type":"server","url":null}},
    {"path":"config/options.json","sha256":"dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd","size":0,"source":{"type":"embedded"}},
    {"path":"mods/changed.jar","sha256":"cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc","size":30,"source":{"type":"server","url":"http://127.0.0.1:9/descriptive-only"}}
  ]}
}"#;

const GOLDEN_INSTALLED: &[u8] = br#"{
  "schemaVersion":1,"version":"1.0.0","revision":4,"files":[
    {"path":"mods/old.jar","sha256":"eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee"},
    {"path":"mods/keep.jar","sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"},
    {"path":"mods/changed.jar","sha256":"ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"}
  ]
}"#;

fn metadata(revision: u64, minimum: u64) -> ManifestMetadata {
    ManifestMetadata::new("Pack", "2.0.0", revision, minimum).unwrap()
}

fn file(path: &str, hash: &str) -> PackFile {
    PackFile::new(path, hash, 1, FileSource::embedded()).unwrap()
}

fn manifest(files: Vec<PackFile>) -> Manifest {
    Manifest::new(metadata(5, 3), PackContent::Custom(files)).unwrap()
}

fn inventory(
    paths: &[(&str, &str)],
    version: Option<&str>,
    revision: Option<u64>,
) -> InstalledState {
    InstalledState::new(
        version,
        revision,
        paths
            .iter()
            .map(|(path, hash)| InstalledFile::new(path, hash).unwrap())
            .collect(),
    )
    .unwrap()
}

fn empty_manifest_json() -> &'static str {
    r#"{"schemaVersion":1,"name":"Pack","version":"2.0.0","revision":5,"minimumRevision":3,"content":{"mode":"custom","files":[]}}"#
}

#[test]
fn golden_complete_diff_retains_metadata_and_old_hash_preconditions() {
    let desired = Manifest::decode(GOLDEN_MANIFEST).unwrap();
    let installed = InstalledState::decode(GOLDEN_INSTALLED).unwrap();
    let result = plan(&desired, &installed).unwrap();
    let actual: Vec<_> = result
        .actions()
        .iter()
        .map(|action| {
            (
                action.operation(),
                action.path().as_str(),
                action.installed_sha256().map(Sha256::to_hex),
                action.desired_file().map(|file| file.sha256().to_hex()),
            )
        })
        .collect();
    assert_eq!(
        actual,
        vec![
            (
                SyncOperation::Add,
                "config/options.json",
                None,
                Some(D.into())
            ),
            (
                SyncOperation::Update,
                "mods/changed.jar",
                Some(F.into()),
                Some(C.into())
            ),
            (
                SyncOperation::Keep,
                "mods/keep.jar",
                Some(A.into()),
                Some(A.into())
            ),
            (SyncOperation::Add, "mods/new.jar", None, Some(B.into())),
            (SyncOperation::Remove, "mods/old.jar", Some(E.into()), None),
        ]
    );
    assert_eq!(result.target_version(), "2.0.0");
    assert_eq!(result.target_revision(), 5);
    assert_eq!(result.minimum_revision(), 3);
    assert_eq!(result.revision_status(), RevisionStatus::Behind);
    assert!(result.minimum_revision_satisfied());
    assert!(!result.version_matches());
    assert!(result.has_file_changes());
    assert!(result.needs_update());

    let changed = result.actions()[1].desired_file().unwrap();
    assert_eq!(changed.size(), 30);
    assert_eq!(changed.source().kind(), SourceKind::Server);
    // Private-network descriptions survive as data, not as authorized requests.
    assert_eq!(
        changed.source().url(),
        Some("http://127.0.0.1:9/descriptive-only")
    );
    let keep = result.actions()[2].desired_file().unwrap();
    assert_eq!(keep.sha1().unwrap()[..4], [0x01, 0x23, 0x45, 0x67]);
    let new = result.actions()[3].desired_file().unwrap();
    assert_eq!(new.source().platform_name(), Some(Platform::Modrinth));
    assert_eq!(new.source().project_id(), Some("project-1"));
    assert_eq!(new.source().version_id(), Some("version_2"));
    assert_eq!(new.source().url(), Some("https://example.invalid/new.jar"));
}

#[test]
fn missing_inventory_proposes_only_sorted_adds() {
    let result = plan(
        &Manifest::decode(GOLDEN_MANIFEST).unwrap(),
        &inventory(&[], None, None),
    )
    .unwrap();
    assert_eq!(
        result
            .actions()
            .iter()
            .map(|a| (a.operation(), a.path().as_str()))
            .collect::<Vec<_>>(),
        vec![
            (SyncOperation::Add, "config/options.json"),
            (SyncOperation::Add, "mods/changed.jar"),
            (SyncOperation::Add, "mods/keep.jar"),
            (SyncOperation::Add, "mods/new.jar"),
        ]
    );
    assert_eq!(result.revision_status(), RevisionStatus::Unverified);
    assert!(!result.minimum_revision_satisfied());
    assert!(
        result
            .actions()
            .iter()
            .all(|a| a.installed_sha256().is_none())
    );
}

#[test]
fn exact_match_keeps_entries_without_needing_update() {
    let desired = Manifest::decode(GOLDEN_MANIFEST).unwrap();
    let installed = inventory(
        &[
            ("mods/new.jar", B),
            ("mods/keep.jar", A),
            ("mods/changed.jar", C),
            ("config/options.json", D),
        ],
        Some("2.0.0"),
        Some(5),
    );
    let result = plan(&desired, &installed).unwrap();
    assert_eq!(result.actions().len(), 4);
    assert!(
        result
            .actions()
            .iter()
            .all(|a| a.operation() == SyncOperation::Keep)
    );
    assert!(!result.needs_update());
    assert!(!result.has_file_changes());
    assert!(result.version_matches());
    assert!(result.minimum_revision_satisfied());
}

#[test]
fn same_version_and_revision_never_hide_hash_drift() {
    let result = plan(
        &manifest(vec![file("mods/a.jar", A)]),
        &inventory(&[("mods/a.jar", B)], Some("2.0.0"), Some(5)),
    )
    .unwrap();
    assert!(result.needs_update());
    assert!(result.has_file_changes());
    assert_eq!(result.actions()[0].operation(), SyncOperation::Update);
    assert_eq!(result.revision_status(), RevisionStatus::Current);
}

#[test]
fn explicit_empty_catalogue_proposes_removals_and_empty_union_is_valid() {
    let desired = manifest(vec![]);
    let result = plan(
        &desired,
        &inventory(&[("z.txt", A), ("a.txt", B)], Some("2.0.0"), Some(5)),
    )
    .unwrap();
    assert_eq!(
        result
            .actions()
            .iter()
            .map(|a| (a.operation(), a.path().as_str()))
            .collect::<Vec<_>>(),
        vec![
            (SyncOperation::Remove, "a.txt"),
            (SyncOperation::Remove, "z.txt")
        ]
    );
    assert!(result.actions().iter().all(|a| a.desired_file().is_none()));
    let empty = plan(&desired, &inventory(&[], Some("2.0.0"), Some(5))).unwrap();
    assert!(empty.actions().is_empty());
    assert!(!empty.needs_update());
}

#[test]
fn proposals_are_input_order_independent_and_versions_are_exact_labels() {
    let left = manifest(vec![file("z.txt", A), file("a.txt", B)]);
    let right = manifest(vec![file("a.txt", B), file("z.txt", A)]);
    let first = inventory(&[("z.txt", A), ("a.txt", B)], Some("2.0"), Some(5));
    let second = inventory(&[("a.txt", B), ("z.txt", A)], Some("2.0"), Some(5));
    assert_eq!(left, right);
    assert_eq!(first, second);
    assert_eq!(plan(&left, &first), plan(&right, &second));
    let result = plan(&left, &first).unwrap();
    assert!(result.needs_update());
    assert!(!result.has_file_changes());
    assert!(!result.version_matches());
    assert_eq!(result.actions()[0].path().as_str(), "a.txt");
}

#[test]
fn revision_policy_including_minimum_boundary_and_unknown_is_explicit() {
    let desired = manifest(vec![file("a.txt", A)]);
    let cases = [
        (None, RevisionStatus::Unverified, false, true),
        (Some(0), RevisionStatus::BelowMinimum, false, true),
        (Some(2), RevisionStatus::BelowMinimum, false, true),
        (Some(3), RevisionStatus::Behind, true, true),
        (Some(4), RevisionStatus::Behind, true, true),
        (Some(5), RevisionStatus::Current, true, false),
    ];
    for (revision, status, minimum_satisfied, needs_update) in cases {
        let result = plan(
            &desired,
            &inventory(&[("a.txt", A)], Some("2.0.0"), revision),
        )
        .unwrap();
        assert_eq!(result.revision_status(), status);
        assert_eq!(result.minimum_revision_satisfied(), minimum_satisfied);
        assert_eq!(result.needs_update(), needs_update);
        assert!(!result.has_file_changes());
        assert_eq!(result.actions()[0].operation(), SyncOperation::Keep);
    }
    assert_eq!(
        plan(&desired, &inventory(&[], Some("2.0.0"), Some(6))),
        Err(SyncError::InstalledRevisionAhead)
    );
    let first = Manifest::new(metadata(1, 1), PackContent::Custom(vec![])).unwrap();
    let unknown = plan(&first, &inventory(&[], Some("2.0.0"), None)).unwrap();
    assert_eq!(unknown.revision_status(), RevisionStatus::Unverified);
    assert!(!unknown.minimum_revision_satisfied());
    assert!(unknown.needs_update());
    let zero = plan(&first, &inventory(&[], Some("2.0.0"), Some(0))).unwrap();
    assert_eq!(zero.revision_status(), RevisionStatus::BelowMinimum);
    assert!(!zero.minimum_revision_satisfied());
    assert!(zero.needs_update());
    assert!(
        !plan(&first, &inventory(&[], Some("2.0.0"), Some(1)))
            .unwrap()
            .needs_update()
    );
}

#[test]
fn revision_values_and_minimum_are_validated_before_planning() {
    assert_eq!(
        ManifestMetadata::new("Pack", "v", 5, 6),
        Err(CatalogueError::InvalidRevision)
    );
    for (target, minimum) in [(0, 0), (1, 0), (0, 1)] {
        assert_eq!(
            ManifestMetadata::new("Pack", "v", target, minimum),
            Err(CatalogueError::InvalidRevision)
        );
    }
    for (revision, minimum) in [(0, 0), (5, 0)] {
        let text = empty_manifest_json()
            .replace("\"revision\":5", &format!("\"revision\":{revision}"))
            .replace(
                "\"minimumRevision\":3",
                &format!("\"minimumRevision\":{minimum}"),
            );
        assert_eq!(
            Manifest::decode(text.as_bytes()),
            Err(CatalogueError::InvalidRevision)
        );
    }
    assert!(ManifestMetadata::new("Pack", "v", MAX_REVISION, MAX_REVISION).is_ok());
    assert_eq!(
        ManifestMetadata::new("Pack", "v", MAX_REVISION + 1, 0),
        Err(CatalogueError::InvalidRevision)
    );
    assert_eq!(
        InstalledState::new(None, Some(MAX_REVISION + 1), vec![]),
        Err(CatalogueError::InvalidRevision)
    );
    let text = empty_manifest_json().replace("\"revision\":5", "\"revision\":-1");
    assert_eq!(
        Manifest::decode(text.as_bytes()),
        Err(CatalogueError::InvalidJson)
    );
    let text = empty_manifest_json().replace("\"revision\":5", "\"revision\":5.0");
    assert_eq!(
        Manifest::decode(text.as_bytes()),
        Err(CatalogueError::InvalidJson)
    );
}

#[test]
fn paths_reject_unsafe_or_ambiguous_spellings_without_normalization() {
    let invalid = [
        "",
        "/a",
        "a/",
        "a//b",
        ".",
        "..",
        "a/./b",
        "a/../b",
        "../a",
        "a\\b",
        "a\\..\\b",
        "C:/a",
        "//server/share",
        "\\\\server\\share",
        "a:b",
        "a\0b",
        "a\nb",
        "a\tb",
        "a ",
        " a",
        "mods/CON .jar",
        "mods/COM9 .jar",
        "a.",
        "a?/b",
        "a*/b",
        "a%2fb",
        "a%2e%2e",
        "é.txt",
        "e\u{301}.txt",
        "a\u{202e}b",
        "CON",
        "con.txt",
        "PRN.log",
        "aux",
        "NuL.bin",
        "CLOCK$",
        "config/CONIN$.txt",
        "CONOUT$",
        "COM0",
        "com0.txt",
        "mods/CoM0.jar",
        "LPT0",
        "lpt0.log",
        "config/LpT0/options.json",
        "COM1",
        "com9.zip",
        "lpt1.txt",
        "LPT9",
    ];
    for path in invalid {
        assert_eq!(
            RelativePath::parse(path),
            Err(CatalogueError::InvalidPath),
            "accepted {path:?}"
        );
        assert_eq!(
            InstalledFile::new(path, A),
            Err(CatalogueError::InvalidPath)
        );
    }
    for path in [
        "mods/Good-1_2.jar",
        "mods/fabric-api-0.161.0+26.3.jar",
        "mods/mielon's-the-sift-1.1.1-mc26.3-fabric.jar",
        "mods/[Fabric] Mod Name (1.21.1).jar",
        "config/a b.json",
        "config/forgeendertech/Biomes/minecraft$mutated_ice_flats.cfg",
        "mods/Level Up! 2-1.1.23-1.12.jar",
        "config/.hidden",
        "a/b/c",
        "COM10.txt",
        "company.txt",
    ] {
        assert_eq!(RelativePath::parse(path).unwrap().as_str(), path);
    }
}

#[test]
fn path_component_depth_and_byte_budgets_have_asserted_boundaries() {
    let component = "a".repeat(MAX_COMPONENT_BYTES);
    assert!(RelativePath::parse(&component).is_ok());
    assert_eq!(
        RelativePath::parse(&(component.clone() + "a")),
        Err(CatalogueError::InvalidPath)
    );
    let exact_depth = vec!["a"; MAX_PATH_COMPONENTS].join("/");
    assert!(RelativePath::parse(&exact_depth).is_ok());
    assert_eq!(
        RelativePath::parse(&(exact_depth + "/a")),
        Err(CatalogueError::InvalidPath)
    );
    // 7*128 + 7 separators + 121 = exactly 1024 bytes, with every component valid.
    let exact_path = format!("{}/{}", [component.as_str(); 7].join("/"), "b".repeat(121));
    assert_eq!(exact_path.len(), MAX_PATH_BYTES);
    assert!(RelativePath::parse(&exact_path).is_ok());
    assert_eq!(
        RelativePath::parse(&(exact_path + "b")),
        Err(CatalogueError::InvalidPath)
    );
}

#[test]
fn full_hashes_are_required_and_case_is_canonicalized_as_bytes() {
    assert_eq!(
        Sha256::parse(A).unwrap(),
        Sha256::parse(&A.to_ascii_uppercase()).unwrap()
    );
    assert_eq!(Sha256::parse(&A.to_ascii_uppercase()).unwrap().to_hex(), A);
    assert_eq!(Sha256::parse(A).unwrap().as_bytes(), &[0xaa; 32]);
    for bad in [
        "",
        "abc",
        "old-hash",
        " aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "gggggggggggggggggggggggggggggggggggggggggggggggggggggggggggggggg",
    ] {
        assert_eq!(Sha256::parse(bad), Err(CatalogueError::InvalidHash));
        assert_eq!(
            InstalledFile::new("a.txt", bad),
            Err(CatalogueError::InvalidHash)
        );
    }
    assert_eq!(
        file("a.txt", A).with_sha1("abc"),
        Err(CatalogueError::InvalidHash)
    );
    assert!(file("a.txt", A).with_sha1(&"F".repeat(40)).is_ok());
}

#[test]
fn duplicate_alias_and_file_directory_collisions_fail_in_both_individual_states() {
    for paths in [
        ["mods/a.jar", "mods/a.jar"],
        ["mods/a.jar", "mods/A.jar"],
        ["mods/a.jar", "Mods/b.jar"],
        ["mods", "mods/a.jar"],
        ["MODS", "mods/a.jar"],
    ] {
        for pair in [paths, [paths[1], paths[0]]] {
            assert_eq!(
                Manifest::new(
                    metadata(5, 3),
                    PackContent::Custom(pair.iter().map(|p| file(p, A)).collect())
                ),
                Err(CatalogueError::PathCollision)
            );
            assert_eq!(
                InstalledState::new(
                    None,
                    None,
                    pair.iter()
                        .map(|p| InstalledFile::new(p, A).unwrap())
                        .collect()
                ),
                Err(CatalogueError::PathCollision)
            );
        }
    }
    // Similar byte prefixes without a slash are not directory conflicts.
    assert!(
        Manifest::new(
            metadata(5, 3),
            PackContent::Custom(vec![file("mods", A), file("mods-other/a.jar", B)])
        )
        .is_ok()
    );
}

#[test]
fn union_collisions_reject_case_only_renames_and_file_directory_transitions() {
    for pair in [
        ["mods/a.jar", "mods/A.jar"],
        ["mods/a.jar", "Mods/b.jar"],
        ["mods", "mods/a.jar"],
        ["mods/a", "mods/a/b.jar"],
    ] {
        for [desired_path, installed_path] in [pair, [pair[1], pair[0]]] {
            assert_eq!(
                plan(
                    &manifest(vec![file(desired_path, A)]),
                    &inventory(&[(installed_path, B)], Some("2.0.0"), Some(5))
                ),
                Err(SyncError::PathCollision)
            );
        }
    }
    // Equal exact names across states are legitimate comparisons, not duplicates.
    assert!(
        plan(
            &manifest(vec![file("mods/a.jar", A)]),
            &inventory(&[("mods/a.jar", B)], None, None)
        )
        .is_ok()
    );
}

#[test]
fn text_and_source_budgets_are_bytes_and_descriptions_are_not_authority() {
    assert!(ManifestMetadata::new(&"é".repeat(MAX_TEXT_BYTES / 2), "v", 1, 1).is_ok());
    assert_eq!(
        ManifestMetadata::new(&"é".repeat(MAX_TEXT_BYTES / 2 + 1), "v", 1, 1),
        Err(CatalogueError::InvalidText)
    );
    for bad in ["", " v", "v ", "v\n", "v\0"] {
        assert_eq!(
            ManifestMetadata::new("Pack", bad, 1, 1),
            Err(CatalogueError::InvalidText)
        );
        assert_eq!(
            InstalledState::new(Some(bad), None, vec![]),
            Err(CatalogueError::InvalidText)
        );
    }
    assert!(
        FileSource::platform(
            Platform::Curseforge,
            &"p".repeat(MAX_SOURCE_ID_BYTES),
            "1",
            None
        )
        .is_ok()
    );
    assert_eq!(
        FileSource::platform(
            Platform::Modrinth,
            &"p".repeat(MAX_SOURCE_ID_BYTES + 1),
            "1",
            None
        ),
        Err(CatalogueError::InvalidSource)
    );
    for bad in ["", "a b", "../", "id\n", "é"] {
        assert_eq!(
            FileSource::platform(Platform::Modrinth, bad, "1", None),
            Err(CatalogueError::InvalidSource)
        );
    }
    let exact_url = "u".repeat(MAX_SOURCE_URL_BYTES);
    assert_eq!(
        FileSource::server(Some(&exact_url)).unwrap().url(),
        Some(exact_url.as_str())
    );
    assert_eq!(
        FileSource::server(Some(&(exact_url + "u"))),
        Err(CatalogueError::InvalidSource)
    );
    assert_eq!(
        FileSource::server(Some("https://example.invalid/\nsecret")),
        Err(CatalogueError::InvalidSource)
    );
    // The policy is deliberately not a scheme/SSRF/download authorization check.
    assert_eq!(
        FileSource::server(Some("file:///descriptive-only"))
            .unwrap()
            .url(),
        Some("file:///descriptive-only")
    );
}

#[test]
fn file_sizes_total_size_and_file_count_are_bounded() {
    assert!(PackFile::new("a.bin", A, 0, FileSource::embedded()).is_ok());
    assert!(PackFile::new("a.bin", A, MAX_FILE_SIZE, FileSource::embedded()).is_ok());
    assert_eq!(
        PackFile::new("a.bin", A, MAX_FILE_SIZE + 1, FileSource::embedded()),
        Err(CatalogueError::InvalidSize)
    );
    let at_size_limit: Vec<_> = (0..MAX_TOTAL_SIZE / MAX_FILE_SIZE)
        .map(|n| {
            PackFile::new(
                &format!("{n}.bin"),
                A,
                MAX_FILE_SIZE,
                FileSource::embedded(),
            )
            .unwrap()
        })
        .collect();
    assert!(Manifest::new(metadata(5, 3), PackContent::Custom(at_size_limit.clone())).is_ok());
    let mut over = at_size_limit;
    over.push(file("extra.bin", A));
    assert_eq!(
        Manifest::new(metadata(5, 3), PackContent::Custom(over)),
        Err(CatalogueError::BudgetExceeded)
    );
    let files: Vec<_> = (0..MAX_FILES)
        .map(|n| file(&format!("{n}.bin"), A))
        .collect();
    assert!(Manifest::new(metadata(5, 3), PackContent::Custom(files.clone())).is_ok());
    let installed: Vec<_> = (0..MAX_FILES)
        .map(|n| InstalledFile::new(&format!("{n}.bin"), A).unwrap())
        .collect();
    assert!(InstalledState::new(None, None, installed.clone()).is_ok());
    let mut over = files;
    over.push(file("extra.bin", A));
    assert_eq!(
        Manifest::new(metadata(5, 3), PackContent::Custom(over)),
        Err(CatalogueError::TooManyFiles)
    );
    let mut over = installed;
    over.push(InstalledFile::new("extra.bin", A).unwrap());
    assert_eq!(
        InstalledState::new(None, None, over),
        Err(CatalogueError::TooManyFiles)
    );
}

#[test]
fn aggregate_text_budgets_cannot_be_bypassed_by_individually_valid_objects() {
    let source = FileSource::server(Some(&"u".repeat(MAX_SOURCE_URL_BYTES))).unwrap();
    let files = (0..=MAX_TOTAL_TEXT_BYTES/MAX_SOURCE_URL_BYTES)
        .map(|n| PackFile::new(&format!("{n}.bin"), A, 0, source.clone()).unwrap())
        .collect();
    assert_eq!(
        Manifest::new(metadata(5, 3), PackContent::Custom(files)),
        Err(CatalogueError::BudgetExceeded)
    );
    let long_directory = vec!["d".repeat(120); 7].join("/");
    let files = (0..=MAX_TOTAL_TEXT_BYTES/long_directory.len())
        .map(|n| InstalledFile::new(&format!("{long_directory}/{n}.bin"), A).unwrap())
        .collect();
    assert_eq!(
        InstalledState::new(None, None, files),
        Err(CatalogueError::BudgetExceeded)
    );
}

#[test]
fn platform_and_archive_catalogues_are_descriptive_and_never_remove_all() {
    let remote = Manifest::new(
        metadata(5, 3),
        PackContent::Platform(
            PlatformPack::new(
                Platform::Modrinth,
                "project",
                "version",
                Some("https://example.invalid/pack"),
            )
            .unwrap(),
        ),
    )
    .unwrap();
    let archive = Manifest::new(
        metadata(5, 3),
        PackContent::Archive(
            ArchivePack::new("packs/pack.zip", A, 123, ArchiveFormat::Zip, None).unwrap(),
        ),
    )
    .unwrap();
    let installed = inventory(&[("mods/a.jar", A)], None, None);
    assert_eq!(
        plan(&remote, &installed),
        Err(SyncError::UnsupportedContent)
    );
    assert_eq!(
        plan(&archive, &installed),
        Err(SyncError::UnsupportedContent)
    );
    let archive_json = format!(
        r#"{{"schemaVersion":1,"name":"Pack","version":"v","revision":1,"minimumRevision":1,"content":{{"mode":"archive","path":"packs/pack.mrpack","sha256":"{A}","size":123,"format":"mrpack","url":null}}}}"#
    );
    let parsed = Manifest::decode(archive_json.as_bytes()).unwrap();
    let PackContent::Archive(pack) = parsed.content() else {
        panic!("wrong content")
    };
    assert_eq!(pack.path().as_str(), "packs/pack.mrpack");
    assert_eq!(pack.sha256().to_hex(), A);
    assert_eq!(pack.size(), 123);
    assert_eq!(pack.format(), ArchiveFormat::Mrpack);
    assert_eq!(pack.url(), None);
    let platform_json = r#"{"schemaVersion":1,"name":"Pack","version":"v","revision":1,"minimumRevision":1,"content":{"mode":"platform","platform":"curseforge","projectId":"1","versionId":"2"}}"#;
    assert!(Manifest::decode(platform_json.as_bytes()).is_ok());
    let mixed = platform_json.replace("\"versionId\":\"2\"", "\"versionId\":\"2\",\"files\":[]");
    assert_eq!(
        Manifest::decode(mixed.as_bytes()),
        Err(CatalogueError::InvalidJson)
    );
}

#[test]
fn decoders_reject_oversized_input_before_parsing_even_when_whitespace() {
    let exact = [
        empty_manifest_json().as_bytes(),
        &vec![b' '; MAX_JSON_BYTES - empty_manifest_json().len()],
    ]
    .concat();
    assert_eq!(exact.len(), MAX_JSON_BYTES);
    assert!(Manifest::decode(&exact).is_ok());
    let oversized = vec![b' '; MAX_JSON_BYTES + 1];
    assert_eq!(
        Manifest::decode(&oversized),
        Err(CatalogueError::InputTooLarge)
    );
    assert_eq!(
        InstalledState::decode(&oversized),
        Err(CatalogueError::InputTooLarge)
    );
}

#[test]
fn strict_manifest_schema_rejects_malformed_data_unknown_and_missing_fields() {
    let empty = empty_manifest_json();
    for bad in [
        String::new(),
        "null".into(),
        "[]".into(),
        "{}".into(),
        "{".into(),
        format!("{empty}{{}}"),
        format!("\u{feff}{empty}"),
        empty.replace("\"name\":\"Pack\",", ""),
        empty.replace("\"revision\":5,", ""),
        empty.replace("\"files\":[]", "\"files\":null"),
        empty.replace("\"version\":\"2.0.0\"", "\"version\":null"),
        empty.replace("\"schemaVersion\":1", "\"schemaVersion\":\"1\""),
        empty.replace("\"schemaVersion\":1", "\"manifestVersion\":1"),
        empty.replace("\"name\":\"Pack\"", "\"name\":\"Pack\",\"admin\":true"),
        empty.replace("\"mode\":\"custom\"", "\"mode\":\"unknown\""),
        empty.replace("\"files\":[]", "\"files\":[],\"unexpected\":false"),
        empty.replace("\"minimumRevision\":3", "\"minimumRevision\":6"),
    ] {
        assert!(Manifest::decode(bad.as_bytes()).is_err(), "accepted {bad}");
    }
    assert_eq!(
        Manifest::decode(&[0xff, 0xfe]),
        Err(CatalogueError::InvalidJson)
    );
    let deep = format!("{}0{}", "[".repeat(140), "]".repeat(140));
    assert_eq!(
        Manifest::decode(deep.as_bytes()),
        Err(CatalogueError::InvalidJson)
    );
    let unsupported = empty.replace("\"schemaVersion\":1", "\"schemaVersion\":2");
    assert_eq!(
        Manifest::decode(unsupported.as_bytes()),
        Err(CatalogueError::UnsupportedSchema)
    );
    // Existing projection/wire objects are not silently accepted as validated input.
    assert_eq!(
        Manifest::decode(br#"{"formatVersion":1,"files":[]}"#),
        Err(CatalogueError::InvalidJson)
    );
}

#[test]
fn every_duplicate_json_key_is_rejected_including_null_and_escaped_keys() {
    let empty = empty_manifest_json();
    for bad in [
        empty.replace(
            "\"version\":\"2.0.0\"",
            "\"version\":\"2.0.0\",\"version\":\"other\"",
        ),
        empty.replace(
            "\"version\":\"2.0.0\"",
            "\"version\":\"2.0.0\",\"\\u0076ersion\":\"other\"",
        ),
        empty.replace(
            "\"mode\":\"custom\"",
            "\"mode\":\"custom\",\"mode\":\"custom\"",
        ),
        String::from_utf8(GOLDEN_MANIFEST.to_vec())
            .unwrap()
            .replace("\"url\":null", "\"url\":null,\"url\":\"secret\""),
        String::from_utf8(GOLDEN_MANIFEST.to_vec())
            .unwrap()
            .replace("\"size\":20", "\"size\":20,\"size\":20"),
    ] {
        assert_eq!(
            Manifest::decode(bad.as_bytes()),
            Err(CatalogueError::InvalidJson)
        );
    }
    for bad in [
        r#"{"schemaVersion":1,"version":null,"version":"secret","files":[]}"#,
        r#"{"schemaVersion":1,"revision":null,"revision":0,"files":[]}"#,
        r#"{"schemaVersion":1,"revision":null,"revision":null,"files":[]}"#,
    ] {
        assert_eq!(
            InstalledState::decode(bad.as_bytes()),
            Err(CatalogueError::InvalidJson)
        );
    }
}

#[test]
fn invalid_file_and_installed_data_fail_instead_of_skipping_or_last_write_wins() {
    let good = String::from_utf8(GOLDEN_MANIFEST.to_vec()).unwrap();
    for bad in [
        good.replace("\"size\":20", "\"size\":-1"),
        good.replace("\"size\":20", "\"size\":1.5"),
        good.replace("\"size\":20", "\"size\":null"),
        good.replace("\"type\":\"server\"", "\"type\":\"unknown\""),
        good.replace("\"platform\":\"modrinth\"", "\"platform\":\"other\""),
        good.replace("\"projectId\":\"project-1\",", ""),
        good.replace(
            "\"type\":\"embedded\"",
            "\"type\":\"embedded\",\"url\":\"secret\"",
        ),
        good.replace("\"size\":20", "\"size\":20,\"permission\":\"write\""),
        good.replace("mods/new.jar", "../outside.jar"),
        good.replace(
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            "bbb",
        ),
        good.replace("0123456789abcdef0123456789abcdef01234567", "sha1"),
        good.replace("mods/new.jar", "mods/keep.jar"),
    ] {
        assert!(Manifest::decode(bad.as_bytes()).is_err());
    }
    for bad in [
        r#"{"schemaVersion":1,"files":null}"#.to_owned(),
        r#"{"schemaVersion":1,"files":[{"path":"a","sha256":null}]}"#.to_owned(),
        r#"{"schemaVersion":1,"files":[{"path":"","sha256":"old-hash"}]}"#.to_owned(),
        format!(
            r#"{{"schemaVersion":1,"files":[{{"path":"a","sha256":"{A}"}},{{"path":"a","sha256":"{B}"}}]}}"#
        ),
        format!(r#"{{"schemaVersion":1,"files":[{{"path":"a","sha256":"{A}","source":null}}]}}"#),
        format!(
            r#"{{"schemaVersion":1,"files":[{{"path":"a","sha256":"{A}"}},{{"path":"A","sha256":"{A}"}}]}}"#
        ),
        format!(r#"{{"schemaVersion":1,"files":[{{"path":"../a","sha256":"{A}"}}]}}"#),
    ] {
        assert!(InstalledState::decode(bad.as_bytes()).is_err());
    }
    assert!(InstalledState::decode(br#"{"schemaVersion":1,"files":[]}"#).is_ok());
}

#[test]
fn decoder_collection_limits_are_enforced_at_the_boundary() {
    let entries: Vec<_> = (0..MAX_FILES)
        .map(|n| format!(r#"{{"path":"{n}.bin","sha256":"{A}"}}"#))
        .collect();
    let exact = format!(r#"{{"schemaVersion":1,"files":[{}]}}"#, entries.join(","));
    assert!(exact.len() < MAX_JSON_BYTES);
    assert_eq!(
        InstalledState::decode(exact.as_bytes())
            .unwrap()
            .files()
            .len(),
        MAX_FILES
    );
    let over = format!(
        r#"{{"schemaVersion":1,"files":[{},{{"path":"extra.bin","sha256":"{A}"}}]}}"#,
        entries.join(",")
    );
    assert_eq!(
        InstalledState::decode(over.as_bytes()),
        Err(CatalogueError::InvalidJson)
    );
    let custom_entries: Vec<_> = (0..=MAX_FILES)
        .map(|n| {
            format!(
                r#"{{"path":"{n}.bin","sha256":"{A}","size":0,"source":{{"type":"embedded"}}}}"#
            )
        })
        .collect();
    let over = empty_manifest_json().replace(
        "\"files\":[]",
        &format!("\"files\":[{}]", custom_entries.join(",")),
    );
    assert!(over.len() < MAX_JSON_BYTES);
    assert_eq!(
        Manifest::decode(over.as_bytes()),
        Err(CatalogueError::InvalidJson)
    );
}

#[test]
fn errors_are_fixed_redacted_and_have_no_input_bearing_causes() {
    let input =
        br#"{"schemaVersion":1,"version":null,"version":"password-secret-token","files":[]}"#;
    let error = InstalledState::decode(input).unwrap_err();
    assert_eq!(error, CatalogueError::InvalidJson);
    assert_eq!(error.to_string(), "Pack catalogue failed: invalid_json");
    assert_eq!(format!("{error:?}"), "InvalidJson");
    assert!(error.source().is_none());
    let collision = plan(
        &manifest(vec![file("mods/secret.jar", A)]),
        &inventory(&[("Mods/other.jar", B)], None, None),
    )
    .unwrap_err();
    assert_eq!(
        collision.to_string(),
        "Pack sync planning failed: path_collision"
    );
    assert_eq!(format!("{collision:?}"), "PathCollision");
    assert!(collision.source().is_none());
    let source = FileSource::server(Some("https://example.invalid/password-secret-token")).unwrap();
    assert!(!format!("{source:?}").contains("password-secret-token"));
}
