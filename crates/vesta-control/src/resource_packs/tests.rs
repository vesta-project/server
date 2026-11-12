use super::*;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File},
    io::{Cursor, Read, Write},
    sync::{Mutex, MutexGuard},
};
use vesta_state::StateDirectory;
use zip::{ZipArchive, ZipWriter, write::SimpleFileOptions};

pub(super) static SERIAL: Mutex<()> = Mutex::new(());
struct Fixture {
    _serial: MutexGuard<'static, ()>,
    temp: tempfile::TempDir,
    state: StateDirectory,
}
impl Fixture {
    fn new() -> Self {
        let serial = SERIAL.lock().unwrap_or_else(|error| error.into_inner());
        let temp = tempfile::tempdir().unwrap();
        let state = StateDirectory::new(temp.path().join("state"), None).unwrap();
        state.prepare().unwrap();
        Self {
            _serial: serial,
            temp,
            state,
        }
    }
    fn input(&self, id: &str, entries: &[(&str, &[u8])]) -> Input {
        self.with_metadata(
            id,
            entries,
            json!({"pack":{"pack_format":46,"description":"Input"}}),
        )
    }
    fn with_metadata(&self, id: &str, entries: &[(&str, &[u8])], metadata: Value) -> Input {
        let meta = serde_json::to_vec(&metadata).unwrap();
        let mut all = vec![("pack.mcmeta", meta.as_slice())];
        all.extend_from_slice(entries);
        self.raw(id, &zip(&all))
    }
    fn raw(&self, id: &str, bytes: &[u8]) -> Input {
        let path = self.temp.path().join(format!("{id}.zip"));
        fs::write(&path, bytes).unwrap();
        Input::new(
            id.into(),
            File::open(path).unwrap(),
            hex::encode(Sha256::digest(bytes)),
        )
        .unwrap()
    }
    fn build(&self, inputs: Vec<Input>) -> BuiltPack {
        build(
            &self.state,
            inputs,
            PackFormat::new(46, 0).unwrap(),
            "Combined",
            BuildLimits::default(),
        )
        .unwrap()
    }
    fn read(&self, pack: &PublishedPack, path: &str) -> Vec<u8> {
        let file = pack
            .open_verified(&self.state.artifacts().unwrap())
            .unwrap();
        let mut zip = ZipArchive::new(file).unwrap();
        let mut bytes = Vec::new();
        zip.by_name(path).unwrap().read_to_end(&mut bytes).unwrap();
        bytes
    }
    fn json(&self, pack: &PublishedPack, path: &str) -> Value {
        serde_json::from_slice(&self.read(pack, path)).unwrap()
    }
}
fn zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
    for (name, bytes) in entries {
        zip.start_file(
            *name,
            SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Stored)
                .last_modified_time(zip::DateTime::default())
                .unix_permissions(0o644)
                .system(zip::System::Unix),
        )
        .unwrap();
        zip.write_all(bytes).unwrap();
    }
    zip.finish().unwrap().into_inner()
}
fn failure(result: Result<BuiltPack>) -> Error {
    match result {
        Err(error) => error,
        Ok(_) => panic!("unexpected build success"),
    }
}

#[test]
fn ordinary_conflicts_follow_priority_and_reordering_changes_winner() {
    let f = Fixture::new();
    let path = "assets/demo/textures/test.png";
    let pack = f
        .build(vec![
            f.input("base", &[(path, b"base")]),
            f.input("higher", &[(path, b"higher")]),
        ])
        .publish()
        .unwrap();
    assert_eq!(f.read(&pack, path), b"higher");
    assert_eq!(pack.report().resolved.len(), 1);
    assert_eq!(pack.report().resolved[0].layers, ["base", "higher"]);
    assert_eq!(
        pack.report().resolved[0].resolution,
        Resolution::HigherPriority
    );
    let reversed = f
        .build(vec![
            f.input("higher", &[(path, b"higher")]),
            f.input("base", &[(path, b"base")]),
        ])
        .publish()
        .unwrap();
    assert_eq!(f.read(&reversed, path), b"base");
    assert_ne!(pack.sha256(), reversed.sha256());
}

#[test]
fn identical_builds_are_deterministic_and_publication_is_idempotent() {
    let f = Fixture::new();
    let first = f
        .build(vec![f.input("one", &[("assets/demo/test.txt", b"data")])])
        .publish()
        .unwrap();
    let second = f
        .build(vec![f.input("one", &[("assets/demo/test.txt", b"data")])])
        .publish()
        .unwrap();
    assert_eq!(first.sha256(), second.sha256());
    assert_eq!(first.sha1(), second.sha1());
    assert_eq!(first.size(), second.size());
    println!("resource_pack_hierarchy_fixture_sha256={}", first.sha256());
    assert_eq!(f.state.artifacts().unwrap().list(4096).unwrap().len(), 1);
}

#[test]
fn language_keys_merge_with_higher_priority_values_and_lower_unique_keys() {
    let f = Fixture::new();
    let path = "assets/demo/lang/en_us.json";
    let pack = f
        .build(vec![
            f.input("base", &[(path, br#"{"low":"kept","shared":"old"}"#)]),
            f.input("top", &[(path, br#"{"top":"kept","shared":"new"}"#)]),
        ])
        .publish()
        .unwrap();
    assert_eq!(
        f.json(&pack, path),
        json!({"low":"kept","top":"kept","shared":"new"})
    );
}

#[test]
fn sound_events_append_without_changing_existing_subtitle_and_keep_unique_events() {
    let f = Fixture::new();
    let path = "assets/demo/sounds.json";
    let pack = f
        .build(vec![
            f.input(
                "base",
                &[(
                    path,
                    br#"{"event":{"subtitle":"lower","sounds":["a"]},"unique":{"sounds":["u"]}}"#,
                )],
            ),
            f.input(
                "top",
                &[(
                    path,
                    br#"{"event":{"subtitle":"higher","sounds":["b","b"]}}"#,
                )],
            ),
        ])
        .publish()
        .unwrap();
    assert_eq!(
        f.json(&pack, path),
        json!({"event":{"subtitle":"lower","sounds":["a","b","b"]},"unique":{"sounds":["u"]}})
    );
}

#[test]
fn sound_replace_resets_sounds_and_subtitle_and_preserves_replace_against_vanilla_base() {
    let f = Fixture::new();
    let path = "assets/demo/sounds.json";
    let pack = f
        .build(vec![
            f.input(
                "base",
                &[(path, br#"{"event":{"subtitle":"old","sounds":["a"]}}"#)],
            ),
            f.input(
                "middle",
                &[(path, br#"{"event":{"replace":true,"sounds":["b"]}}"#)],
            ),
            f.input(
                "top",
                &[(path, br#"{"event":{"subtitle":"ignored","sounds":["c"]}}"#)],
            ),
        ])
        .publish()
        .unwrap();
    assert_eq!(
        f.json(&pack, path),
        json!({"event":{"replace":true,"sounds":["b","c"]}})
    );
}

#[test]
fn atlas_sources_keep_low_to_high_execution_order_including_filters() {
    let f = Fixture::new();
    let path = "assets/demo/atlases/blocks.json";
    let pack = f.build(vec![f.input("base", &[(path, br#"{"sources":[{"type":"single","resource":"demo:a"}]}"#)]),
        f.input("top", &[(path, br#"{"sources":[{"type":"filter","pattern":{"path":"a"}},{"type":"single","resource":"demo:b"}]}"#)])]).publish().unwrap();
    assert_eq!(
        f.json(&pack, path)["sources"],
        json!([{"type":"single","resource":"demo:a"},{"type":"filter","pattern":{"path":"a"}},{"type":"single","resource":"demo:b"}])
    );
}

#[test]
fn fonts_keep_higher_pack_first_with_each_pack_provider_order_and_lower_fallbacks() {
    let f = Fixture::new();
    let path = "assets/demo/font/test.json";
    let pack = f.build(vec![f.input("base", &[(path, br#"{"providers":[{"type":"reference","id":"demo:a1"},{"type":"reference","id":"demo:a2"}]}"#)]),
        f.input("top", &[(path, br#"{"providers":[{"type":"reference","id":"demo:b1"},{"type":"reference","id":"demo:b2"}]}"#)])]).publish().unwrap();
    let ids: Vec<_> = f.json(&pack, path)["providers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["id"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(ids, ["demo:b1", "demo:b2", "demo:a1", "demo:a2"]);
}

#[test]
fn metadata_is_generated_for_target_and_notices_are_retained_per_input() {
    let f = Fixture::new();
    let pack = f
        .build(vec![
            f.input(
                "base",
                &[("LICENSE.txt", b"base notice"), ("pack.png", b"base icon")],
            ),
            f.input(
                "top",
                &[("LICENSE.txt", b"top notice"), ("pack.png", b"top icon")],
            ),
        ])
        .publish()
        .unwrap();
    assert_eq!(
        f.read(&pack, "vesta/attribution/base/LICENSE.txt"),
        b"base notice"
    );
    assert_eq!(
        f.read(&pack, "vesta/attribution/top/LICENSE.txt"),
        b"top notice"
    );
    assert_eq!(f.read(&pack, "pack.png"), b"top icon");
    assert_eq!(
        f.json(&pack, "pack.mcmeta")["pack"]["description"],
        "Combined"
    );
    assert_eq!(f.json(&pack, "vesta/stack.json")["inputs"][0]["id"], "base");
}

#[test]
fn missing_metadata_invalid_json_and_incompatible_formats_refuse() {
    let f = Fixture::new();
    let target = PackFormat::new(46, 0).unwrap();
    let run = |input| {
        failure(build(
            &f.state,
            vec![input],
            target,
            "Combined",
            BuildLimits::default(),
        ))
    };
    assert_eq!(
        run(f.raw("missing", &zip(&[("assets/demo/x.txt", b"data")]))),
        Error::InvalidMetadata
    );
    assert_eq!(
        run(f.raw("invalid", &zip(&[("pack.mcmeta", b"{")]))),
        Error::InvalidMetadata
    );
    assert_eq!(
        run(f.with_metadata(
            "wrong",
            &[],
            json!({"pack":{"pack_format":34,"description":"old"}})
        )),
        Error::IncompatibleFormat
    );
}

#[test]
fn declared_legacy_ranges_and_modern_minor_ranges_are_checked() {
    for value in [
        json!({"pack":{"pack_format":34,"supported_formats":[34,46],"description":"range"}}),
        json!({"pack":{"pack_format":34,"supported_formats":{"min_inclusive":34,"max_inclusive":46},"description":"range"}}),
    ] {
        assert!(semantic::metadata(&value, PackFormat::new(46, 0).unwrap()).is_ok());
    }
    let modern = json!({"pack":{"min_format":[69,1],"max_format":[69,3],"description":"minor"}});
    assert!(semantic::metadata(&modern, PackFormat::new(69, 2).unwrap()).is_ok());
    assert_eq!(
        semantic::metadata(&modern, PackFormat::new(69, 0).unwrap()),
        Err(Error::IncompatibleFormat)
    );
}

#[test]
fn modern_scalar_maxima_include_all_minors_and_generated_metadata_omits_forbidden_legacy_ranges() {
    let f = Fixture::new();
    let target = PackFormat::new(69, 7).unwrap();
    for maximum in [json!(69), json!([69]), json!([69, 9])] {
        let metadata =
            json!({"pack":{"min_format":[69],"max_format":maximum,"description":"modern"}});
        assert!(semantic::metadata(&metadata, target).is_ok());
    }
    let invalid = json!({"pack":{"min_format":69,"max_format":69,"supported_formats":69,"description":"invalid"}});
    assert_eq!(
        semantic::metadata(&invalid, target),
        Err(Error::InvalidMetadata)
    );
    let metadata = json!({"pack":{"min_format":69,"max_format":69,"description":"modern"}});
    let pack = build(
        &f.state,
        vec![f.with_metadata("modern", &[], metadata)],
        target,
        "Combined",
        BuildLimits::default(),
    )
    .unwrap()
    .publish()
    .unwrap();
    let output = f.json(&pack, "pack.mcmeta");
    assert!(output["pack"].get("supported_formats").is_none());
    assert!(output["pack"].get("pack_format").is_none());
    assert_eq!(output["pack"]["min_format"], json!([69, 7]));
    assert_eq!(output["pack"]["max_format"], json!([69, 7]));
}

#[test]
fn active_pack_filters_and_overlays_are_explicitly_unsupported_not_silently_ignored() {
    let f = Fixture::new();
    for key in ["filter", "overlays"] {
        let mut metadata = json!({"pack":{"pack_format":46,"description":"Input"}});
        metadata[key] = if key == "filter" {
            json!({"block":[{"namespace":"demo"}]})
        } else {
            json!({"entries":[{"directory":"new","formats":46}]})
        };
        let input = f.with_metadata(key, &[], metadata);
        assert_eq!(
            failure(build(
                &f.state,
                vec![input],
                PackFormat::new(46, 0).unwrap(),
                "Combined",
                BuildLimits::default()
            )),
            Error::UnsupportedMetadata
        );
    }
    assert_eq!(f.state.artifacts().unwrap().count(4096).unwrap(), Some(0));
}

#[test]
fn unsafe_names_and_duplicate_archive_paths_refuse() {
    let f = Fixture::new();
    for name in [
        "../escape.txt",
        "/absolute.txt",
        "C:/drive.txt",
        "assets/../escape.txt",
        "assets\\demo\\bad.txt",
        "assets/demo/CON.txt",
    ] {
        let input = f.input("unsafe", &[(name, b"data")]);
        assert_eq!(
            failure(build(
                &f.state,
                vec![input],
                PackFormat::new(46, 0).unwrap(),
                "Combined",
                BuildLimits::default()
            )),
            Error::InvalidArchive,
            "{name}"
        );
    }
    let mut bytes = zip(&[
        (
            "pack.mcmeta",
            br#"{"pack":{"pack_format":46,"description":"Input"}}"#,
        ),
        ("one.txt", b"one"),
        ("two.txt", b"two"),
    ]);
    for at in 0..bytes.len() - 7 {
        if &bytes[at..at + 7] == b"two.txt" {
            bytes[at..at + 7].copy_from_slice(b"one.txt");
        }
    }
    assert_eq!(
        failure(build(
            &f.state,
            vec![f.raw("duplicates", &bytes)],
            PackFormat::new(46, 0).unwrap(),
            "Combined",
            BuildLimits::default()
        )),
        Error::InvalidArchive
    );
}

#[test]
fn digest_mismatch_and_zip64_marker_refuse_before_output_creation() {
    let f = Fixture::new();
    let mut changed = f.input("changed", &[]);
    changed.sha256 = "0".repeat(64);
    assert_eq!(
        failure(build(
            &f.state,
            vec![changed],
            PackFormat::new(46, 0).unwrap(),
            "Combined",
            BuildLimits::default()
        )),
        Error::ChangedInput
    );
    let mut bytes = zip(&[("pack.mcmeta", b"{}")]);
    let eocd = bytes.len() - 22;
    bytes[eocd + 10..eocd + 12].copy_from_slice(&u16::MAX.to_le_bytes());
    assert_eq!(
        failure(build(
            &f.state,
            vec![f.raw("zip64", &bytes)],
            PackFormat::new(46, 0).unwrap(),
            "Combined",
            BuildLimits::default()
        )),
        Error::InvalidArchive
    );
}

#[test]
fn member_expansion_and_output_budgets_fail_and_keep_existing_artifacts() {
    let f = Fixture::new();
    let saved = f
        .build(vec![f.input("saved", &[("assets/demo/kept.txt", b"kept")])])
        .publish()
        .unwrap();
    let input = f.input("large", &[("assets/demo/data.bin", &[0u8; 2048])]);
    assert_eq!(
        failure(build(
            &f.state,
            vec![input],
            PackFormat::new(46, 0).unwrap(),
            "Combined",
            BuildLimits {
                member_bytes: 1024,
                ..Default::default()
            }
        )),
        Error::TooLarge
    );
    let input = f.input("output", &[("assets/demo/x.txt", b"data")]);
    assert_eq!(
        failure(build(
            &f.state,
            vec![input],
            PackFormat::new(46, 0).unwrap(),
            "Combined",
            BuildLimits {
                output_bytes: 32,
                ..Default::default()
            }
        )),
        Error::TooLarge
    );
    assert_eq!(f.read(&saved, "assets/demo/kept.txt"), b"kept");
    assert_eq!(f.state.artifacts().unwrap().count(4096).unwrap(), Some(1));
}

#[test]
fn failed_semantic_merge_cleans_stage_and_later_build_can_run() {
    let f = Fixture::new();
    let input = f.input("malformed", &[("assets/demo/lang/en_us.json", b"[]")]);
    assert_eq!(
        failure(build(
            &f.state,
            vec![input],
            PackFormat::new(46, 0).unwrap(),
            "Combined",
            BuildLimits::default()
        )),
        Error::InvalidMetadata
    );
    assert_eq!(f.state.artifacts().unwrap().count(4096).unwrap(), Some(0));
    let good = f.build(vec![f.input("good", &[("assets/demo/good.txt", b"good")])]);
    drop(good);
    assert_eq!(f.state.artifacts().unwrap().count(4096).unwrap(), Some(0));
}

#[test]
fn deadline_and_duplicate_input_ids_fail_without_starting_background_work() {
    let f = Fixture::new();
    let input = f.input("time", &[]);
    assert_eq!(
        failure(build(
            &f.state,
            vec![input],
            PackFormat::new(46, 0).unwrap(),
            "Combined",
            BuildLimits {
                time: std::time::Duration::from_nanos(1),
                ..Default::default()
            }
        )),
        Error::TimedOut
    );
    assert_eq!(
        failure(build(
            &f.state,
            vec![f.input("same", &[]), f.input("same", &[])],
            PackFormat::new(46, 0).unwrap(),
            "Combined",
            BuildLimits::default()
        )),
        Error::InvalidInput
    );
}

#[test]
fn symlinks_corrupt_members_and_forged_entry_counts_refuse() {
    let f = Fixture::new();
    let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
    writer
        .start_file("pack.mcmeta", SimpleFileOptions::default())
        .unwrap();
    writer
        .write_all(br#"{"pack":{"pack_format":46,"description":"Input"}}"#)
        .unwrap();
    writer
        .add_symlink(
            "assets/demo/link",
            "../../outside",
            SimpleFileOptions::default(),
        )
        .unwrap();
    let input = f.raw("linked", &writer.finish().unwrap().into_inner());
    assert_eq!(
        failure(build(
            &f.state,
            vec![input],
            PackFormat::new(46, 0).unwrap(),
            "Combined",
            BuildLimits::default()
        )),
        Error::InvalidArchive
    );

    let mut bytes = zip(&[
        (
            "pack.mcmeta",
            br#"{"pack":{"pack_format":46,"description":"Input"}}"#,
        ),
        ("assets/demo/x.txt", b"corrupt-me"),
    ]);
    let at = bytes
        .windows(10)
        .position(|bytes| bytes == b"corrupt-me")
        .unwrap();
    bytes[at] ^= 1;
    assert_eq!(
        failure(build(
            &f.state,
            vec![f.raw("crc", &bytes)],
            PackFormat::new(46, 0).unwrap(),
            "Combined",
            BuildLimits::default()
        )),
        Error::InvalidArchive
    );

    let mut bytes = zip(&[("pack.mcmeta", b"{}")]);
    let at = bytes.len() - 22;
    bytes[at + 8..at + 10].copy_from_slice(&10000u16.to_le_bytes());
    bytes[at + 10..at + 12].copy_from_slice(&10000u16.to_le_bytes());
    assert_eq!(
        failure(build(
            &f.state,
            vec![f.raw("count", &bytes)],
            PackFormat::new(46, 0).unwrap(),
            "Combined",
            BuildLimits::default()
        )),
        Error::TooLarge
    );
}

#[test]
fn total_input_budgets_include_hidden_lower_priority_members() {
    let f = Fixture::new();
    let inputs = || {
        vec![
            f.input("low", &[("assets/demo/x.txt", &[b'a'; 200])]),
            f.input("high", &[("assets/demo/x.txt", &[b'b'; 200])]),
        ]
    };
    for limits in [
        BuildLimits {
            expanded_bytes: 300,
            ..Default::default()
        },
        BuildLimits {
            entries: 3,
            ..Default::default()
        },
        BuildLimits {
            compressed_bytes: 10,
            ..Default::default()
        },
    ] {
        assert_eq!(
            failure(build(
                &f.state,
                inputs(),
                PackFormat::new(46, 0).unwrap(),
                "Combined",
                limits
            )),
            Error::TooLarge
        );
    }
    assert_eq!(f.state.artifacts().unwrap().count(4096).unwrap(), Some(0));
}

#[test]
fn merged_json_is_bounded_even_when_each_input_fits() {
    let f = Fixture::new();
    let low = serde_json::to_vec(&json!({"low":"a".repeat(600)})).unwrap();
    let high = serde_json::to_vec(&json!({"high":"b".repeat(600)})).unwrap();
    let inputs = vec![
        f.input("low", &[("assets/demo/lang/en_us.json", &low)]),
        f.input("high", &[("assets/demo/lang/en_us.json", &high)]),
    ];
    assert_eq!(
        failure(build(
            &f.state,
            inputs,
            PackFormat::new(46, 0).unwrap(),
            "Combined",
            BuildLimits {
                json_bytes: 1000,
                ..Default::default()
            }
        )),
        Error::TooLarge
    );
    assert_eq!(f.state.artifacts().unwrap().count(4096).unwrap(), Some(0));
}

#[test]
fn changed_stages_are_rejected_before_content_addressing_and_existing_artifacts_are_never_replaced()
{
    let f = Fixture::new();
    let built = f.build(vec![f.input("one", &[])]);
    let name = f
        .state
        .artifacts()
        .unwrap()
        .list(4096)
        .unwrap()
        .pop()
        .unwrap();
    fs::write(
        f.state.path().join("artifacts").join(name.as_str()),
        b"outside change",
    )
    .unwrap();
    assert!(matches!(built.publish(), Err(Error::ChangedInput)));
    assert_eq!(f.state.artifacts().unwrap().count(4096).unwrap(), Some(0));

    let published = f.build(vec![f.input("one", &[])]).publish().unwrap();
    let built = f.build(vec![f.input("one", &[])]);
    let path = f
        .state
        .path()
        .join("artifacts")
        .join(published.artifact_name());
    fs::write(&path, b"outside change").unwrap();
    assert!(matches!(built.publish(), Err(Error::ChangedInput)));
    assert_eq!(fs::read(path).unwrap(), b"outside change");
    assert_eq!(f.state.artifacts().unwrap().count(4096).unwrap(), Some(1));
}
