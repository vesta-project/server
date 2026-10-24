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
