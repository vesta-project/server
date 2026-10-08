use serde_json::{Value, json};
use vesta_protocol::pack::{
    FORMAT, FORMAT_VERSION, INDEX_NAME, PackProjectionError,
    model::{BuilderFile, BuilderFileSource, BuilderManifest, BuilderModloader},
    project_index,
};

const GOLDEN_INDEX: &[u8] =
    include_bytes!("../../../core/src/test/resources/vesta-builder-v1.json");

fn manifest_with_files(mods: Vec<BuilderFile>, overrides: Vec<BuilderFile>) -> BuilderManifest {
    BuilderManifest {
        name: Some("Test pack".into()),
        version: Some("1.0".into()),
        revision: 1,
        minimum_revision: 0,
        minecraft_version: Some("1.21.1".into()),
        modloader: Some(BuilderModloader {
            r#type: Some("fabric".into()),
            version: Some("0.16.0".into()),
        }),
        mods: Some(mods),
        overrides: Some(overrides),
    }
}

fn manifest_file(path: Option<&str>, size: i64) -> BuilderFile {
    BuilderFile {
        path: path.map(str::to_owned),
        sha1: None,
        sha256: None,
        size,
        source: None,
    }
}

fn fixture_manifest(expected: &Value) -> BuilderManifest {
    let files = expected["files"]
        .as_array()
        .expect("fixture files array")
        .iter()
        .map(|wire| {
            let hashes = wire["hashes"].as_object().expect("fixture hashes object");
            let source = wire["source"].as_object().expect("fixture source object");
            BuilderFile {
                path: wire["path"].as_str().map(str::to_owned),
                size: wire["size"].as_i64().expect("fixture signed file size"),
                sha1: hashes
                    .get("sha1")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                sha256: hashes
                    .get("sha256")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                source: Some(BuilderFileSource {
                    r#type: source
                        .get("type")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                    platform: source
                        .get("platform")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                    project_id: source
                        .get("project_id")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                    version_id: source
                        .get("version_id")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                    url: source.get("url").and_then(Value::as_str).map(str::to_owned),
                }),
            }
        })
        .collect();

    BuilderManifest {
        name: expected["name"].as_str().map(str::to_owned),
        version: expected["version"].as_str().map(str::to_owned),
        revision: expected["revision"].as_i64().expect("fixture revision"),
        minimum_revision: expected["minimumRevision"]
            .as_i64()
            .expect("fixture minimum revision"),
        minecraft_version: expected["minecraftVersion"].as_str().map(str::to_owned),
        modloader: Some(BuilderModloader {
            r#type: expected["modloaderType"].as_str().map(str::to_owned),
            version: expected["modloaderVersion"].as_str().map(str::to_owned),
        }),
        mods: Some(files),
        overrides: Some(Vec::new()),
    }
}

#[test]
fn projection_matches_the_unchanged_java_interoperability_fixture_semantically() {
    let expected: Value = serde_json::from_slice(GOLDEN_INDEX).expect("Java golden fixture JSON");
    let manifest = fixture_manifest(&expected);
    let before = manifest.clone();

    let first = project_index(&manifest).expect("fixture projection");
    let second = project_index(&manifest).expect("repeat fixture projection");

    assert_eq!(
        manifest, before,
        "projection must not mutate caller-owned arrays"
    );
    assert_eq!(first, second, "repeated projection must be stable");
    assert_eq!(
        serde_json::to_value(first).expect("serialized index"),
        expected
    );
    assert_eq!(INDEX_NAME, "vesta.index.json");
    assert_eq!(FORMAT, "vesta");
    assert_eq!(FORMAT_VERSION, 1);
}

#[test]
fn projection_sorts_combined_files_like_java_without_mutating_inputs() {
    let manifest = manifest_with_files(
        vec![
            manifest_file(Some("mods/\u{e000}.jar"), 1),
            manifest_file(Some("mods/z.jar"), 2),
            manifest_file(Some("mods/\u{10000}.jar"), 3),
        ],
        vec![manifest_file(Some("config/a.txt"), 4)],
    );
    let before = manifest.clone();

    let index = project_index(&manifest).expect("valid paths");

    let paths: Vec<&str> = index.files.iter().map(|file| file.path.as_str()).collect();
    assert_eq!(
        paths,
        [
            "config/a.txt",
            "mods/z.jar",
            "mods/\u{10000}.jar",
            "mods/\u{e000}.jar",
        ],
        "ordering follows Java String.compareTo UTF-16 code units",
    );
    assert_eq!(
        manifest, before,
        "projection must not reorder caller arrays"
    );
}

#[test]
fn optional_index_fields_are_omitted_and_absent_source_projects_as_embedded() {
    let manifest = BuilderManifest {
        name: None,
        version: None,
        revision: -7,
        minimum_revision: 9,
        minecraft_version: None,
        modloader: None,
        mods: Some(vec![manifest_file(Some("config/options.txt"), 8)]),
        overrides: None,
    };

    let value = serde_json::to_value(project_index(&manifest).expect("projection"))
        .expect("index JSON value");

    assert!(value.get("name").is_none());
    assert!(value.get("version").is_none());
    assert!(value.get("minecraftVersion").is_none());
    assert!(value.get("modloaderType").is_none());
    assert!(value.get("modloaderVersion").is_none());
    assert_eq!(value["dependencies"], json!({}));
    assert_eq!(
        value["files"][0],
        json!({
            "path": "config/options.txt",
            "size": 8,
            "hashes": {},
            "env": {"client": "required", "server": "required"},
            "source": {"type": "embedded"}
        })
    );
}

#[test]
fn mixed_case_known_provider_is_external_and_platform_falls_back_to_original_type() {
    let mut file = manifest_file(Some("mods/provider.jar"), 5);
    file.source = Some(BuilderFileSource {
        r#type: Some("MoDrInTh".into()),
        platform: None,
        project_id: Some("project".into()),
        version_id: Some("version".into()),
        url: Some("https://example.invalid/file.jar".into()),
    });
    let manifest = manifest_with_files(vec![file], Vec::new());

    let index = project_index(&manifest).expect("projection");

    assert_eq!(
        index.files[0].source.r#type, "MoDrInTh",
        "the builder preserves the source type's original spelling",
    );
    assert_eq!(index.files[0].source.platform.as_deref(), Some("MoDrInTh"));
    assert_eq!(index.files[0].source.project_id.as_deref(), Some("project"));
    assert_eq!(index.files[0].source.version_id.as_deref(), Some("version"));
    assert_eq!(
        index.files[0].source.url.as_deref(),
        Some("https://example.invalid/file.jar"),
    );
}

#[test]
fn unknown_source_types_are_projected_as_embedded() {
    let mut file = manifest_file(Some("mods/manual.jar"), 7);
    file.source = Some(BuilderFileSource {
        r#type: Some("server-local".into()),
        platform: Some("custom-platform".into()),
        project_id: Some("not-a-provider-project".into()),
        version_id: Some("not-a-provider-version".into()),
        url: Some("https://example.invalid/not-followed".into()),
    });
    let manifest = manifest_with_files(vec![file], Vec::new());

    let index = project_index(&manifest).expect("projection");

    assert_eq!(
        serde_json::to_value(&index.files[0].source).expect("source JSON"),
        json!({"type": "embedded"}),
    );
}

#[test]
fn every_recognized_loader_has_a_dependency_with_empty_missing_version() {
    for (loader_type, dependency_key) in [
        ("FaBrIc", "fabric-loader"),
        ("QuIlT", "quilt-loader"),
        ("FoRgE", "forge"),
        ("NeOfOrGe", "neoforge"),
    ] {
        let mut manifest = manifest_with_files(Vec::new(), Vec::new());
        manifest.minecraft_version = None;
        manifest.modloader = Some(BuilderModloader {
            r#type: Some(loader_type.into()),
            version: None,
        });

        let index = project_index(&manifest).expect("recognized loader projection");
        assert_eq!(
            serde_json::to_value(index.dependencies).expect("dependency JSON"),
            json!({dependency_key: ""}),
            "missing loader versions become empty strings",
        );
        assert_eq!(index.modloader_type.as_deref(), Some(loader_type));
        assert!(index.modloader_version.is_none());
    }
}

#[test]
fn unknown_loader_does_not_add_a_dependency() {
    let mut manifest = manifest_with_files(Vec::new(), Vec::new());
    manifest.minecraft_version = None;
    manifest.modloader = Some(BuilderModloader {
        r#type: Some("unknown-loader".into()),
        version: Some("9.9".into()),
    });

    let index = project_index(&manifest).expect("unknown loader remains inert");

    assert_eq!(
        serde_json::to_value(index.dependencies).expect("dependency JSON"),
        json!({}),
    );
    assert_eq!(index.modloader_type.as_deref(), Some("unknown-loader"));
    assert_eq!(index.modloader_version.as_deref(), Some("9.9"));
}

#[test]
fn signed_java_long_boundaries_are_preserved() {
    let mut manifest = manifest_with_files(
        vec![manifest_file(Some("mods/extreme.jar"), i64::MAX)],
        Vec::new(),
    );
    manifest.revision = i64::MIN;
    manifest.minimum_revision = i64::MAX;

    let index = project_index(&manifest).expect("signed boundary values project");

    assert_eq!(index.revision, i64::MIN);
    assert_eq!(index.minimum_revision, i64::MAX);
    assert_eq!(index.files[0].size, i64::MAX);
}

#[test]
fn null_entry_path_returns_the_fixed_projection_error_instead_of_panicking() {
    let manifest = manifest_with_files(vec![manifest_file(None, 0)], Vec::new());

    let result = std::panic::catch_unwind(|| project_index(&manifest));

    assert!(result.is_ok(), "null path projection must not panic");
    assert_eq!(
        result.expect("projection did not panic"),
        Err(PackProjectionError)
    );
}
