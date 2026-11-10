use vesta_protocol::pack::{
    catalogue::{
        CatalogueError, FileSource, MAX_FILES, MAX_JSON_BYTES, MAX_TOTAL_TEXT_BYTES, Platform,
        RelativePath, Sha256,
    },
    sources::{KnownSource, SourceRegistry},
};
const HASH: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
fn entry(path: &str) -> KnownSource {
    KnownSource::new(
        path,
        HASH,
        FileSource::platform(Platform::Modrinth, "project", "version", None).unwrap(),
    )
    .unwrap()
}
fn document(path: &str) -> String {
    format!(
        r#"{{"schemaVersion":1,"files":[{{"path":"{path}","sha256":"{HASH}","platform":"modrinth","projectId":"project","versionId":"version","url":null}}]}}"#
    )
}

#[test]
fn independent_registry_golden_resolves_only_exact_path_and_content() {
    let registry = SourceRegistry::decode(document("mods/a.jar").as_bytes()).unwrap();
    let path = RelativePath::parse("mods/a.jar").unwrap();
    let hash = Sha256::parse(HASH).unwrap();
    let source = registry.source_for(&path, &hash).unwrap().unwrap();
    assert_eq!(source.platform_name(), Some(Platform::Modrinth));
    assert_eq!(source.project_id(), Some("project"));
    assert_eq!(source.version_id(), Some("version"));
    assert_eq!(source.url(), None);
    assert_eq!(registry.len(), 1);
    assert!(!registry.is_empty());
    assert!(
        registry
            .source_for(&RelativePath::parse("mods/unknown.jar").unwrap(), &hash)
            .unwrap()
            .is_none()
    );
    assert_eq!(
        registry.source_for(&path, &Sha256::parse(&"00".repeat(32)).unwrap()),
        Err(CatalogueError::InvalidHash)
    );
    assert_eq!(
        registry.source_for(&RelativePath::parse("mods/A.jar").unwrap(), &hash),
        Err(CatalogueError::PathCollision)
    );
}

#[test]
fn missing_malformed_duplicate_unknown_and_positional_records_are_not_empty_registries() {
    let good = document("mods/a.jar");
    let source_array = format!(
        r#"{{"schemaVersion":1,"files":[["mods/a.jar","{HASH}","modrinth","project","version",null]]}}"#
    );
    let bad = [
        "".to_owned(),
        "null".into(),
        "[]".into(),
        "[1,[]]".into(),
        source_array,
        good.replace("\"schemaVersion\":1", "\"schemaVersion\":2"),
        good.replace(
            "\"schemaVersion\":1",
            "\"schemaVersion\":1,\"schemaVersion\":1",
        ),
        good.replace("\"url\":null", "\"url\":null,\"url\":null"),
        good.replace("\"files\":", "\"extra\":true,\"files\":"),
        good.replace("\"url\":null", "\"secret\":\"hidden\""),
        good.replace("modrinth", "server"),
        good.replace(HASH, "invalid"),
        good.replace("mods/a.jar", "../escape"),
        good.replace("\"versionId\":\"version\",", ""),
        good.replace("\"files\":[", "\"files\":null,\"unexpected\":["),
        format!("{good} {{}}"),
    ];
    for input in bad {
        let error = SourceRegistry::decode(input.as_bytes()).unwrap_err();
        assert!(!format!("{error:?} {error}").contains("hidden"));
    }
    assert!(
        SourceRegistry::decode(br#"{"schemaVersion":1,"files":[]}"#)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn registry_namespace_refuses_duplicate_case_and_file_directory_collisions() {
    for paths in [
        ["mods/a.jar", "mods/a.jar"],
        ["mods/a.jar", "mods/A.jar"],
        ["Mods/a.jar", "mods/b.jar"],
        ["mods", "mods/a.jar"],
    ] {
        assert_eq!(
            SourceRegistry::new(paths.into_iter().map(entry).collect()).unwrap_err(),
            CatalogueError::PathCollision
        );
    }
    assert_eq!(
        KnownSource::new("mods/a.jar", HASH, FileSource::embedded()).unwrap_err(),
        CatalogueError::InvalidSource
    );
    assert_eq!(
        KnownSource::new("mods/a.jar", HASH, FileSource::server(None).unwrap()).unwrap_err(),
        CatalogueError::InvalidSource
    );
}

#[test]
fn registry_count_and_input_budgets_are_inclusive_and_no_text_is_debugged() {
    let entries = (0..MAX_FILES)
        .map(|i| entry(&format!("mods/f{i}.jar")))
        .collect();
    let registry = SourceRegistry::new(entries).unwrap();
    assert_eq!(registry.len(), MAX_FILES);
    assert!(!format!("{registry:?}").contains("mods/"));
    assert!(!format!("{:?}", entry("mods/hidden.jar")).contains("hidden"));
    assert_eq!(
        SourceRegistry::new(
            (0..=MAX_FILES)
                .map(|i| entry(&format!("mods/f{i}.jar")))
                .collect()
        )
        .unwrap_err(),
        CatalogueError::TooManyFiles
    );
    let mut exact = br#"{"schemaVersion":1,"files":[]}"#.to_vec();
    exact.resize(MAX_JSON_BYTES, b' ');
    assert!(SourceRegistry::decode(&exact).unwrap().is_empty());
    exact.push(b' ');
    assert_eq!(
        SourceRegistry::decode(&exact).unwrap_err(),
        CatalogueError::InputTooLarge
    );
}

#[test]
fn aggregate_registry_provenance_text_is_bounded_not_just_record_count() {
    let source = FileSource::platform(
        Platform::Curseforge,
        &"p".repeat(128),
        &"v".repeat(128),
        Some(&format!("https://example.invalid/{}", "a".repeat(2000))),
    )
    .unwrap();
    // Exceed the current aggregate text budget without reaching the file-count limit.
    let count = MAX_TOTAL_TEXT_BYTES / 2000 + 1;
    assert!(count < MAX_FILES);
    let entries = (0..count)
        .map(|i| KnownSource::new(&format!("mods/{i}.jar"), HASH, source.clone()).unwrap())
        .collect();
    assert_eq!(
        SourceRegistry::new(entries).unwrap_err(),
        CatalogueError::BudgetExceeded
    );
}
