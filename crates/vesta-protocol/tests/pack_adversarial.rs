use serde_json::json;
use vesta_protocol::pack::{
    model::{BuilderFile, BuilderManifest},
    project_index,
};

fn manifest(mods: Vec<BuilderFile>, overrides: Vec<BuilderFile>) -> BuilderManifest {
    BuilderManifest {
        name: None,
        version: None,
        revision: 1,
        minimum_revision: 0,
        minecraft_version: None,
        modloader: None,
        mods: Some(mods),
        overrides: Some(overrides),
    }
}

fn file(path: &str, marker: i64) -> BuilderFile {
    BuilderFile {
        path: Some(path.into()),
        sha1: None,
        sha256: None,
        size: marker,
        source: None,
    }
}

#[test]
fn equal_paths_retain_mods_then_overrides_order_without_deduplication() {
    let input = manifest(
        vec![file("same/path", 1), file("same/path", 2)],
        vec![file("same/path", 3)],
    );
    let before = input.clone();
    let index = project_index(&input).unwrap();
    assert_eq!(input, before);
    assert_eq!(
        index
            .files
            .iter()
            .map(|entry| entry.size)
            .collect::<Vec<_>>(),
        [1, 2, 3]
    );
}

#[test]
fn source_input_and_index_wire_names_are_distinct_java_models() {
    let input: BuilderManifest = serde_json::from_value(json!({
        "revision": 1,
        "minimumRevision": 0,
        "mods": [{
            "path": "mods/example.jar", "size": 5,
            "source": {"type": "curseforge", "projectId": "project", "versionId": "version"}
        }]
    }))
    .unwrap();
    let input_wire = serde_json::to_value(&input).unwrap();
    assert_eq!(input_wire["mods"][0]["source"]["projectId"], "project");
    assert!(input_wire["mods"][0]["source"].get("project_id").is_none());
    let index = serde_json::to_value(project_index(&input).unwrap()).unwrap();
    let source = &index["files"][0]["source"];
    assert_eq!(source["project_id"], "project");
    assert_eq!(source["version_id"], "version");
    assert!(source.get("projectId").is_none());
    assert!(source.get("versionId").is_none());
    assert!(source.get("url").is_none());
}

#[test]
fn descriptive_projection_does_not_claim_path_or_digest_validation() {
    let mut entry = file("../outside", i64::MIN);
    entry.sha256 = Some("not-a-digest".into());
    let input = manifest(vec![entry], Vec::new());
    let index = project_index(&input).unwrap();
    assert_eq!(index.files[0].path, "../outside");
    assert_eq!(index.files[0].size, i64::MIN);
    assert_eq!(
        index.files[0].hashes.sha256.as_deref(),
        Some("not-a-digest")
    );
    assert_eq!(index.files[0].source.r#type, "embedded");
    // Admission to filesystem, download and installation policy is a separate host boundary.
}
