use super::*;
use crate::{files::FileBrowser, operations::OperationState, world::NewWorld};

fn files(h: &Harness) -> FileBrowser {
    fs::write(h.cwd.join("server.properties"),b"# keep comment\nonline-mode=false\nlevel-name=world\nlevel-seed=\ncustom-property=preserve\n").unwrap();
    fs::create_dir(h.cwd.join("world")).unwrap();
    fs::write(
        h.cwd.join("world/level.dat"),
        crate::world::tests::level(i64::MIN, true),
    )
    .unwrap();
    fs::write(h.cwd.join("world/owned-chunk"), b"preserve exactly").unwrap();
    FileBrowser::open(h.db.clone(), &h.cwd, &[]).unwrap()
}
fn request(files: &FileBrowser, h: &Harness) -> NewWorld {
    let current = files.world(&h.subject).unwrap();
    NewWorld {
        seed: "9223372036854775807".into(),
        expected_revision: current.revision,
        confirm_world: current.name,
    }
}
#[test]
fn offline_datapack_edits_are_guarded_revision_checked_and_replayable() {
    let h = Harness::new(false);
    let files = files(&h);
    fs::write(h.cwd.join("world/level.dat"), crate::world::tests::level_with_packs()).unwrap();
    fs::create_dir_all(h.cwd.join("world/datapacks/disabled")).unwrap();
    let owned = h.runtime(Duration::from_secs(1));
    let original = fs::read(h.cwd.join("world/level.dat")).unwrap();
    let patch = crate::world::OfflineDatapackPatch { pack_id: "file/disabled".into(), enabled: true, expected_revision: files.world(&h.subject).unwrap().revision };
    let result = files.patch_offline_datapack(&owned.runtime.network_handle(), &h.subject, "offline-pack", patch.clone()).unwrap();
    let changed = fs::read(h.cwd.join("world/level.dat")).unwrap();
    assert_ne!(original, changed);
    assert!(files.world(&h.subject).unwrap().details.datapacks.iter().any(|pack| pack.id == "file/disabled" && pack.enabled == Some(true)));
    assert_eq!(files.patch_offline_datapack(&owned.runtime.network_handle(), &h.subject, "offline-pack", patch.clone()).unwrap(), result);
    assert!(files.patch_offline_datapack(&owned.runtime.network_handle(), &h.subject, "stale-pack", patch.clone()).is_err());
    let mut builtin = patch.clone(); builtin.pack_id = "vanilla".into(); builtin.expected_revision = files.world(&h.subject).unwrap().revision;
    assert!(files.patch_offline_datapack(&owned.runtime.network_handle(), &h.subject, "builtin-pack", builtin).is_err());
    assert_eq!(fs::read(h.cwd.join("world/level.dat")).unwrap(), changed);
    let current_revision = files.world(&h.subject).unwrap().revision;
    let start = h.submit(GameCommand::Start);
    h.wait_state(&start, OperationState::Succeeded);
    let running_patch = crate::world::OfflineDatapackPatch { expected_revision: current_revision.clone(), enabled: false, ..patch.clone() };
    assert!(files.patch_offline_datapack(&owned.runtime.network_handle(), &h.subject, "running-pack", running_patch).is_err());
    assert_eq!(fs::read(h.cwd.join("world/level.dat")).unwrap(), changed);
    let stop = h.submit(GameCommand::Stop { expected_generation: owned.runtime.game_snapshot().unwrap().generation.unwrap() });
    h.wait_state(&stop, OperationState::Succeeded);
    h.db.write(|c| { c.execute("DELETE FROM role_assignments WHERE principal_id=?", [h.subject.principal.as_str()])?; Ok(()) }).unwrap();
    let revoked = crate::world::OfflineDatapackPatch { expected_revision: current_revision, enabled: false, ..patch };
    assert!(files.patch_offline_datapack(&owned.runtime.network_handle(), &h.subject, "revoked-pack", revoked).is_err());
    assert_eq!(fs::read(h.cwd.join("world/level.dat")).unwrap(), changed);
}
#[test]
#[ignore = "requires VESTA_WORLD_TEST_ROOT explicitly selecting a world directory; read-only"]
fn selected_world_metadata_is_read_without_modification() {
    let root = std::path::PathBuf::from(
        std::env::var_os("VESTA_WORLD_TEST_ROOT").expect("selected world directory"),
    );
    let level = fs::read(root.join("level.dat")).unwrap();
    crate::world::details(&level).unwrap();
    if crate::world::seed(&level).unwrap().is_none() {
        let path = [
            "dimensions/minecraft/overworld/data/minecraft/world_gen_settings.dat",
            "data/minecraft/world_gen_settings.dat",
        ]
        .into_iter()
        .map(|path| root.join(path))
        .find(|path| path.exists())
        .expect("overworld metadata");
        let data = fs::read(path).unwrap();
        // Never print the selected world's seed or other private metadata.
        assert!(
            !crate::world::world_generation_seed(&data)
                .unwrap()
                .is_empty()
        );
    }
}

#[test]
fn dimension_local_worldgen_metadata_and_dimensions_are_detected_without_software_identity() {
    let h = Harness::new(false);
    let files = files(&h);
    fs::write(
        h.cwd.join("world/level.dat"),
        crate::world::tests::split_level(),
    )
    .unwrap();
    let dimensions = h.cwd.join("world/dimensions/minecraft");
    for dimension in ["overworld", "the_nether", "the_end"] {
        let data = dimensions.join(dimension).join("data/minecraft");
        fs::create_dir_all(&data).unwrap();
        fs::write(
            data.join("world_gen_settings.dat"),
            crate::world::tests::split_worldgen(if dimension == "overworld" { 42 } else { 99 }),
        )
        .unwrap();
    }
    let first = files.world(&h.subject).unwrap();
    assert_eq!(first.seed.as_deref(), Some("42"));
    for dimension in ["overworld", "nether", "end"] {
        assert!(
            first
                .details
                .dimensions
                .iter()
                .any(|d| d.id == dimension && d.present)
        );
    }
    let source = dimensions.join("overworld/data/minecraft/world_gen_settings.dat");
    fs::write(&source, crate::world::tests::split_worldgen(43)).unwrap();
    let next = files.world(&h.subject).unwrap();
    assert_eq!(next.seed.as_deref(), Some("43"));
    assert_ne!(first.revision, next.revision);
    // A stale root-layout file must not override or mask the dimension-local one.
    fs::create_dir_all(h.cwd.join("world/data/minecraft")).unwrap();
    fs::write(
        h.cwd.join("world/data/minecraft/world_gen_settings.dat"),
        crate::world::tests::split_worldgen(7),
    )
    .unwrap();
    fs::write(&source, b"invalid metadata").unwrap();
    assert!(matches!(
        files.world(&h.subject),
        Err(crate::files::FileError::Corrupt)
    ));
    fs::remove_file(source).unwrap();
    fs::remove_file(h.cwd.join("world/data/minecraft/world_gen_settings.dat")).unwrap();
    assert!(matches!(
        files.world(&h.subject),
        Err(crate::files::FileError::Corrupt)
    )); // Never borrow another dimension's seed.
}

#[test]
fn split_worldgen_seed_is_read_and_fenced_in_world_revision() {
    let h = Harness::new(false);
    let files = files(&h);
    fs::write(
        h.cwd.join("world/level.dat"),
        crate::world::tests::split_level(),
    )
    .unwrap();
    let data = h.cwd.join("world/data/minecraft");
    fs::create_dir_all(&data).unwrap();
    let path = data.join("world_gen_settings.dat");
    fs::write(&path, crate::world::tests::split_worldgen(i64::MAX)).unwrap();
    let first = files.world(&h.subject).unwrap();
    assert_eq!(first.seed.as_deref(), Some("9223372036854775807"));
    fs::write(&path, crate::world::tests::split_worldgen(i64::MIN)).unwrap();
    let next = files.world(&h.subject).unwrap();
    assert_eq!(next.seed.as_deref(), Some("-9223372036854775808"));
    assert_ne!(first.revision, next.revision);
    fs::write(&path, b"invalid metadata").unwrap();
    assert!(matches!(
        files.world(&h.subject),
        Err(crate::files::FileError::Corrupt)
    ));
    fs::remove_file(&path).unwrap();
    assert!(matches!(
        files.world(&h.subject),
        Err(crate::files::FileError::Corrupt)
    ));
}
#[test]
fn world_can_be_selected_before_first_minecraft_start() {
    let h = Harness::new(false);
    let files = FileBrowser::open(h.db.clone(), &h.cwd, &[]).unwrap();
    let owned = h.runtime(Duration::from_secs(1));
    let current = files.world(&h.subject).unwrap();
    assert_eq!(current.name, "world");
    assert!(!current.exists);
    assert!(current.seed.is_none());
    let result = files
        .new_world(
            &owned.runtime.network_handle(),
            &h.subject,
            "first-world",
            request(&files, &h),
        )
        .unwrap();
    assert!(result["preserved_worlds"].as_array().unwrap().is_empty());
    assert_eq!(
        serde_json::to_value(files.world(&h.subject).unwrap()).unwrap(),
        result["world"]
    );
}
#[test]
fn world_status_reads_saved_seed_and_new_world_preserves_data_and_replays_exactly() {
    let h = Harness::new(false);
    let files = files(&h);
    let owned = h.runtime(Duration::from_secs(1));
    let current = files.world(&h.subject).unwrap();
    assert_eq!(current.seed, Some(i64::MIN.to_string()));
    assert_eq!(current.configured_seed, "");
    let patch = request(&files, &h);
    let first = files
        .new_world(
            &owned.runtime.network_handle(),
            &h.subject,
            "world-first",
            patch.clone(),
        )
        .unwrap();
    assert_eq!(first["preserved_worlds"], serde_json::json!(["world"]));
    assert_eq!(first["world"]["exists"], false);
    assert_eq!(
        fs::read(h.cwd.join("world/owned-chunk")).unwrap(),
        b"preserve exactly"
    );
    let props = fs::read_to_string(h.cwd.join("server.properties")).unwrap();
    assert!(props.contains("# keep comment"));
    assert!(props.contains("custom-property=preserve"));
    assert!(props.contains("level-seed=9223372036854775807"));
    assert_eq!(
        files
            .new_world(
                &owned.runtime.network_handle(),
                &h.subject,
                "world-first",
                patch.clone()
            )
            .unwrap(),
        first
    );
    assert!(
        files
            .new_world(
                &owned.runtime.network_handle(),
                &h.subject,
                "another-intent",
                patch
            )
            .is_err()
    );
    assert_eq!(
        fs::read_to_string(h.cwd.join("server.properties")).unwrap(),
        props
    );
    assert_eq!(
        serde_json::to_value(files.world(&h.subject).unwrap()).unwrap(),
        first["world"]
    );
}
#[test]
fn world_selection_refuses_wrong_name_running_game_and_revoked_authority() {
    let h = Harness::new(false);
    let files = files(&h);
    let owned = h.runtime(Duration::from_secs(1));
    let mut patch = request(&files, &h);
    patch.confirm_world = "another-world".into();
    let before = fs::read(h.cwd.join("server.properties")).unwrap();
    assert!(
        files
            .new_world(
                &owned.runtime.network_handle(),
                &h.subject,
                "wrong-world",
                patch
            )
            .is_err()
    );
    let patch = request(&files, &h);
    let start = h.submit(GameCommand::Start);
    h.wait_state(&start, OperationState::Succeeded);
    assert!(
        files
            .new_world(
                &owned.runtime.network_handle(),
                &h.subject,
                "live-world",
                patch.clone()
            )
            .is_err()
    );
    assert_eq!(fs::read(h.cwd.join("server.properties")).unwrap(), before);
    let stop = h.submit(GameCommand::Stop {
        expected_generation: owned.runtime.game_snapshot().unwrap().generation.unwrap(),
    });
    h.wait_state(&stop, OperationState::Succeeded);
    h.db.write(|c| {
        c.execute(
            "DELETE FROM role_assignments WHERE principal_id=?",
            [h.subject.principal.as_str()],
        )?;
        Ok(())
    })
    .unwrap();
    assert!(
        files
            .new_world(
                &owned.runtime.network_handle(),
                &h.subject,
                "revoked-world",
                patch
            )
            .is_err()
    );
    assert_eq!(fs::read(h.cwd.join("server.properties")).unwrap(), before);
}

#[test]
fn world_details_include_split_dimensions_pack_states_and_cached_disk_size() {
    let h = Harness::new(false);
    let files = files(&h);
    fs::write(
        h.cwd.join("world/level.dat"),
        crate::world::tests::level_with_packs(),
    )
    .unwrap();
    fs::create_dir_all(h.cwd.join("world_nether/DIM-1/region")).unwrap();
    fs::write(
        h.cwd.join("world_nether/DIM-1/region/r.0.0.mca"),
        [1u8; 1024],
    )
    .unwrap();
    fs::create_dir_all(h.cwd.join("world/datapacks/disabled")).unwrap();
    fs::write(h.cwd.join("world/datapacks/enabled.zip"), b"fixture zip").unwrap();
    fs::write(
        h.cwd.join("world/datapacks/new.zip"),
        b"not yet in level.dat",
    )
    .unwrap();
    let first = files.world(&h.subject).unwrap();
    assert!(first.details.size_complete);
    assert!(
        first
            .details
            .size_bytes
            .as_ref()
            .unwrap()
            .parse::<u64>()
            .unwrap()
            > 1024
    );
    assert_eq!(first.details.last_played_at_ms, Some(1791115200000));
    assert!(
        first
            .details
            .dimensions
            .iter()
            .any(|d| d.id == "nether" && d.present && d.layout == "split")
    );
    assert!(
        first
            .details
            .datapacks
            .iter()
            .any(|p| p.id == "file/enabled.zip" && p.enabled == Some(true))
    );
    assert!(
        first
            .details
            .datapacks
            .iter()
            .any(|p| p.id == "file/disabled" && p.enabled == Some(false))
    );
    assert!(
        first
            .details
            .datapacks
            .iter()
            .any(|p| p.id == "file/new.zip" && p.enabled.is_none())
    );
    fs::write(h.cwd.join("world/new-chunk"), [2u8; 512]).unwrap();
    assert_eq!(
        files.world(&h.subject).unwrap().details.size_bytes,
        first.details.size_bytes
    );
}
