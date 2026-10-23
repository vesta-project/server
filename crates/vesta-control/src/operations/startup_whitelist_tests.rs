use super::*;
use crate::files::{FileBrowser, StartupWhitelistPatch};
use crate::operations::OperationState;

fn files(h: &Harness) -> FileBrowser {
    fs::write(h.cwd.join("server.properties"), b"online-mode=false\n").unwrap();
    fs::write(h.cwd.join("whitelist-fixture"), b"owned").unwrap();
    FileBrowser::open(h.db.clone(), &h.cwd, &[]).unwrap()
}
fn desired(
    files: &FileBrowser,
    h: &Harness,
    handle: &RuntimeHandle,
    g: GenerationId,
    player: &str,
    add: bool,
    key: &str,
) -> String {
    files
        .startup_whitelist_update(
            handle,
            &h.subject,
            key,
            StartupWhitelistPatch {
                player: player.into(),
                add,
                expected_generation: g.to_hex(),
            },
        )
        .unwrap()["intent_id"]
        .as_str()
        .unwrap()
        .into()
}
fn receipt(files: &FileBrowser, h: &Harness, id: &str) -> String {
    files.startup_whitelist_receipts(&h.subject).unwrap()["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == id)
        .unwrap()["status"]
        .as_str()
        .unwrap()
        .into()
}
fn ready(h: &Harness, handle: &RuntimeHandle, g: GenerationId) {
    h.submit(GameCommand::Console {
        expected_generation: g,
        line: "vesta-fixture-ready".into(),
    });
    wait_for(|| {
        handle.readiness().unwrap().readiness == crate::operations::readiness::Readiness::Ready
    });
}
fn writes(h: &Harness) -> String {
    fs::read_to_string(h.cwd.join("operation-commands")).unwrap_or_default()
}
fn await_command(h: &Harness, line: &str) {
    wait_for(|| writes(h).contains(line));
    std::thread::sleep(Duration::from_millis(40));
}

#[test]
fn startup_whitelist_add_remove_cancels_without_touching_game_or_file_and_replays_receipt() {
    let h = Harness::new(false);
    let files = files(&h);
    let owned = h.runtime(Duration::from_secs(1));
    let handle = owned.runtime.network_handle();
    let g = start(&h, &owned.runtime);
    let id = desired(&files, &h, &handle, g, "alex", true, "desired-add");
    assert_eq!(files.projected_whitelist(&handle).unwrap()[0].name, "alex");
    assert!(!h.cwd.join("whitelist.json").exists());
    let remove = desired(&files, &h, &handle, g, "alex", false, "desired-remove");
    assert!(files.projected_whitelist(&handle).unwrap().is_empty());
    assert_eq!(receipt(&files, &h, &id), "cancelled");
    assert_eq!(receipt(&files, &h, &remove), "cancelled");
    assert_eq!(
        desired(&files, &h, &handle, g, "alex", true, "desired-add"),
        id
    );
    assert!(files.projected_whitelist(&handle).unwrap().is_empty());
    files.tick_startup_whitelist(&handle).unwrap();
    ready(&h, &handle, g);
    files.tick_startup_whitelist(&handle).unwrap();
    assert!(!writes(&h).contains("whitelist"));
}
#[test]
fn startup_whitelist_dispatch_waits_for_ready_and_file_confirmation_and_recovers_same_operation() {
    let h = Harness::new(false);
    let files = files(&h);
    let owned = h.runtime(Duration::from_secs(1));
    let handle = owned.runtime.network_handle();
    let g = start(&h, &owned.runtime);
    let id = desired(&files, &h, &handle, g, "alex", true, "verify-add");
    files.tick_startup_whitelist(&handle).unwrap();
    assert!(!writes(&h).contains("whitelist"));
    ready(&h, &handle, g);
    files.tick_startup_whitelist(&handle).unwrap();
    await_command(&h, "whitelist add alex");
    files.tick_startup_whitelist(&handle).unwrap();
    assert_eq!(receipt(&files, &h, &id), "queued");
    // Simulate a crash after operation admission but before persisting its receipt.
    h.db.write(|c|{c.execute("UPDATE settings SET desired_value=json_set(desired_value,'$.entries[0].operation',NULL) WHERE key='whitelist-runtime'",[])?;Ok(())}).unwrap();
    let recovered = FileBrowser::open(h.db.clone(), &h.cwd, &[]).unwrap();
    recovered.tick_startup_whitelist(&handle).unwrap();
    assert_eq!(writes(&h).matches("whitelist add alex").count(), 1);
    let projected = recovered.projected_whitelist(&handle).unwrap();
    fs::write(
        h.cwd.join("whitelist.json"),
        serde_json::to_vec(&projected).unwrap(),
    )
    .unwrap();
    recovered.tick_startup_whitelist(&handle).unwrap();
    assert_eq!(receipt(&recovered, &h, &id), "applied");
}
#[test]
fn startup_whitelist_inverse_after_dispatch_reconciles_in_order_instead_of_cancelling_io() {
    let h = Harness::new(false);
    let files = files(&h);
    let owned = h.runtime(Duration::from_secs(1));
    let handle = owned.runtime.network_handle();
    let g = start(&h, &owned.runtime);
    let add = desired(&files, &h, &handle, g, "alex", true, "flight-add");
    ready(&h, &handle, g);
    files.tick_startup_whitelist(&handle).unwrap();
    await_command(&h, "whitelist add alex");
    let remove = desired(&files, &h, &handle, g, "alex", false, "flight-remove");
    assert!(files.projected_whitelist(&handle).unwrap().is_empty());
    files.tick_startup_whitelist(&handle).unwrap();
    await_command(&h, "whitelist remove alex");
    files.tick_startup_whitelist(&handle).unwrap();
    assert_eq!(receipt(&files, &h, &add), "superseded");
    assert_eq!(receipt(&files, &h, &remove), "applied");
}
#[test]
fn startup_whitelist_mismatch_expires_and_rolls_back_projection() {
    let h = Harness::new(false);
    let files = files(&h);
    let owned = h.runtime(Duration::from_secs(1));
    let handle = owned.runtime.network_handle();
    let g = start(&h, &owned.runtime);
    let id = desired(&files, &h, &handle, g, "alex", true, "mismatch-add");
    ready(&h, &handle, g);
    files.tick_startup_whitelist(&handle).unwrap();
    await_command(&h, "whitelist add alex");
    files.tick_startup_whitelist(&handle).unwrap();
    h.db.write(|c|{c.execute("UPDATE settings SET desired_value=json_set(desired_value,'$.entries[0].verification_since',?) WHERE key='whitelist-runtime'",[h.db.clock().millis()-21000])?;Ok(())}).unwrap();
    assert_eq!(files.tick_startup_whitelist(&handle).unwrap(), vec!["alex"]);
    assert_eq!(receipt(&files, &h, &id), "failed");
    assert!(files.projected_whitelist(&handle).unwrap().is_empty());
}
#[test]
fn startup_whitelist_does_not_block_stop_or_replay_into_another_generation() {
    let h = Harness::new(false);
    let files = files(&h);
    let owned = h.runtime(Duration::from_secs(1));
    let handle = owned.runtime.network_handle();
    let g = start(&h, &owned.runtime);
    let id = desired(&files, &h, &handle, g, "alex", true, "stop-add");
    let stop = h.submit(GameCommand::Stop {
        expected_generation: g,
    });
    h.wait_state(&stop, OperationState::Succeeded);
    assert_eq!(files.tick_startup_whitelist(&handle).unwrap(), vec!["alex"]);
    assert_eq!(receipt(&files, &h, &id), "failed");
    assert!(!writes(&h).contains("whitelist"));
}
#[test]
fn startup_whitelist_current_authority_is_rechecked_before_dispatch() {
    let h = Harness::new(false);
    let files = files(&h);
    let owned = h.runtime(Duration::from_secs(1));
    let handle = owned.runtime.network_handle();
    let g = start(&h, &owned.runtime);
    let id = desired(&files, &h, &handle, g, "alex", true, "revoked-add");
    h.db.write(|c| {
        c.execute(
            "DELETE FROM role_assignments WHERE principal_id=?",
            [h.subject.principal.as_str()],
        )?;
        Ok(())
    })
    .unwrap();
    assert_eq!(files.tick_startup_whitelist(&handle).unwrap(), vec!["alex"]);
    assert!(!writes(&h).contains("whitelist"));
    h.db.write(|c|{c.execute("INSERT INTO role_assignments (id,principal_id,role_id,granted_by,created_at) VALUES (?,?,'owner','fixture',?)",params![uuid::Uuid::new_v4().to_string(),h.subject.principal.as_str(),h.db.clock().millis()])?;Ok(())}).unwrap();
    assert_eq!(receipt(&files, &h, &id), "failed");
}

#[test]
fn startup_whitelist_cap_is_bounded_and_inverse_cancellation_frees_capacity() {
    let h = Harness::new(false);
    let files = files(&h);
    let owned = h.runtime(Duration::from_secs(1));
    let handle = owned.runtime.network_handle();
    let g = start(&h, &owned.runtime);
    for n in 0..32 {
        desired(
            &files,
            &h,
            &handle,
            g,
            &format!("player{n}"),
            true,
            &format!("cap-{n}"),
        );
    }
    assert_eq!(files.projected_whitelist(&handle).unwrap().len(), 32);
    assert!(
        files
            .startup_whitelist_update(
                &handle,
                &h.subject,
                "cap-overflow",
                StartupWhitelistPatch {
                    player: "overflow".into(),
                    add: true,
                    expected_generation: g.to_hex()
                }
            )
            .is_err()
    );
    desired(&files, &h, &handle, g, "player0", false, "cap-cancel");
    desired(&files, &h, &handle, g, "overflow", true, "cap-after-cancel");
    assert_eq!(files.projected_whitelist(&handle).unwrap().len(), 32);
    assert!(!writes(&h).contains("whitelist"));
}

#[test]
fn startup_whitelist_inverse_uses_membership_after_concurrent_completion() {
    let h = Harness::new(false);
    let files = files(&h);
    let owned = h.runtime(Duration::from_secs(1));
    let handle = owned.runtime.network_handle();
    let g = start(&h, &owned.runtime);
    let add = desired(&files, &h, &handle, g, "alex", true, "retiring-add");
    ready(&h, &handle, g);
    files.tick_startup_whitelist(&handle).unwrap();
    await_command(&h, "whitelist add alex");
    let projected = files.projected_whitelist(&handle).unwrap();
    let concurrent = files.clone();
    let runtime = handle.clone();
    let path = h.cwd.join("whitelist.json");
    FileBrowser::before_startup_whitelist_lock_for_test(move || {
        fs::write(path, serde_json::to_vec(&projected).unwrap()).unwrap();
        concurrent.tick_startup_whitelist(&runtime).unwrap();
    });
    let remove = desired(&files, &h, &handle, g, "alex", false, "retiring-remove");
    assert_eq!(receipt(&files, &h, &add), "applied");
    assert_eq!(receipt(&files, &h, &remove), "queued");
    files.tick_startup_whitelist(&handle).unwrap();
    await_command(&h, "whitelist remove alex");
    fs::write(h.cwd.join("whitelist.json"), b"[]").unwrap();
    files.tick_startup_whitelist(&handle).unwrap();
    assert_eq!(receipt(&files, &h, &remove), "applied");
}

#[test]
fn startup_whitelist_recovery_required_retires_projection_without_releasing_recovery_lock() {
    let h = Harness::new(false);
    let files = files(&h);
    let owned = h.runtime(Duration::from_secs(1));
    let handle = owned.runtime.network_handle();
    let g = start(&h, &owned.runtime);
    let add = desired(&files, &h, &handle, g, "alex", true, "recovery-add");
    ready(&h, &handle, g);
    files.tick_startup_whitelist(&handle).unwrap();
    await_command(&h, "whitelist add alex");
    h.db.write(|c| {
        // Construct the complete uncertain-operation state: genuine recovery
        // retains its generation lock, unlike an ordinary succeeded operation.
        c.execute("UPDATE operations SET state='recovery_required',finished_at=NULL,external_effect_started=1 WHERE id=(SELECT json_extract(desired_value,'$.entries[0].operation') FROM settings WHERE key='whitelist-runtime')", [])?;
        c.execute("INSERT INTO resource_locks(resource,operation_id,acquired_at) SELECT 'game.generation',json_extract(desired_value,'$.entries[0].operation'),? FROM settings WHERE key='whitelist-runtime'", [h.db.clock().millis()])?;
        Ok(())
    }).unwrap();
    assert_eq!(files.tick_startup_whitelist(&handle).unwrap(), vec!["alex"]);
    assert_eq!(receipt(&files, &h, &add), "failed");
    assert!(files.projected_whitelist(&handle).unwrap().is_empty());
    let locks: i64 =
        h.db.read(|c| {
            Ok(c.query_row(
                "SELECT count(*) FROM resource_locks WHERE resource='game.generation'",
                [],
                |r| r.get(0),
            )?)
        })
        .unwrap();
    assert_eq!(
        locks, 1,
        "desired-state retirement cannot clear operation recovery"
    );
}

#[test]
fn startup_whitelist_pending_receipts_survive_completed_outcome_retention() {
    let h = Harness::new(false);
    let files = files(&h);
    let owned = h.runtime(Duration::from_secs(1));
    let handle = owned.runtime.network_handle();
    let g = start(&h, &owned.runtime);
    let add = desired(&files, &h, &handle, g, "alex", true, "old-add");
    h.db.write(|c| {
        c.execute(
            "UPDATE management_file_outcomes SET completed_at=? WHERE request_key='old-add'",
            [h.db.clock().millis() - 8 * 24 * 60 * 60 * 1000],
        )?;
        Ok(())
    })
    .unwrap();
    // Every new remote mutation runs the bounded completed-outcome purge.
    desired(&files, &h, &handle, g, "bob", true, "purge-trigger");
    assert_eq!(receipt(&files, &h, &add), "queued");
    ready(&h, &handle, g);
    files.tick_startup_whitelist(&handle).unwrap();
    await_command(&h, "whitelist add alex");
    let projected = files.projected_whitelist(&handle).unwrap();
    fs::write(
        h.cwd.join("whitelist.json"),
        serde_json::to_vec(&projected).unwrap(),
    )
    .unwrap();
    files.tick_startup_whitelist(&handle).unwrap();
    assert_eq!(receipt(&files, &h, &add), "applied");
}

#[test]
fn startup_whitelist_offline_mixed_case_uuid_is_not_verified_by_name() {
    let h = Harness::new(false);
    let files = files(&h);
    let owned = h.runtime(Duration::from_secs(1));
    let handle = owned.runtime.network_handle();
    let g = start(&h, &owned.runtime);
    let id = desired(&files, &h, &handle, g, "Alex", true, "case-sensitive-add");
    ready(&h, &handle, g);
    files.tick_startup_whitelist(&handle).unwrap();
    await_command(&h, "whitelist add Alex");
    // Minecraft/profile caching can persist the lowercase offline identity.
    fs::write(
        h.cwd.join("whitelist.json"),
        br#"[{"name":"Alex","uuid":"bf20048c-a55a-322c-a100-5493e7b87286"}]"#,
    )
    .unwrap();
    files.tick_startup_whitelist(&handle).unwrap();
    assert_eq!(receipt(&files, &h, &id), "queued");
    h.db.write(|c|{c.execute("UPDATE settings SET desired_value=json_set(desired_value,'$.entries[0].verification_since',?) WHERE key='whitelist-runtime'",[h.db.clock().millis()-21000])?;Ok(())}).unwrap();
    assert_eq!(files.tick_startup_whitelist(&handle).unwrap(), vec!["Alex"]);
    assert_eq!(receipt(&files, &h, &id), "failed");
}
