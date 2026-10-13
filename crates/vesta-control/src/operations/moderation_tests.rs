use super::*;
use crate::operations::OperationId;
// Included in the existing owned-inert-child runner fixture module.
#[test]
fn moderation_authority_history_and_native_acknowledgement_are_separate_from_console() {
    use crate::moderation::ModerationAction;
    let harness = Harness::new(false);
    let owned = harness.runtime(Duration::from_secs(1));
    let handle = owned.runtime.network_handle();
    let generation = start(&harness, &owned.runtime);
    harness
        .db
        .write(|c| {
            c.execute(
                "UPDATE role_assignments SET role_id='moderator' WHERE principal_id=?",
                [harness.subject.principal.as_str()],
            )?;
            Ok(())
        })
        .unwrap();
    let command = GameCommand::Moderate {
        expected_generation: generation,
        action: ModerationAction::Kick {
            player: "Alex".into(),
            reason: Some("  griefing  ".into()),
        },
    };
    let admission = handle
        .submit_game(&harness.subject, command.clone(), "typed-kick")
        .unwrap();
    let (crate::operations::Admission::Created(summary)
    | crate::operations::Admission::Existing(summary)) = admission;
    wait_for(|| {
        harness
            .db
            .read(|c| {
                Ok(c.query_row(
                    "SELECT outcome='succeeded' FROM moderation_actions WHERE operation_id=?",
                    [summary.id.as_str()],
                    |r| r.get::<_, bool>(0),
                )?)
            })
            .unwrap()
    });
    assert_eq!(
        fs::read_to_string(harness.cwd.join("operation-commands")).unwrap(),
        "kick Alex griefing\n"
    );
    assert!(matches!(
        handle
            .submit_game(&harness.subject, command, "typed-kick")
            .unwrap(),
        crate::operations::Admission::Existing(_)
    ));
    let history = harness
        .operations
        .moderation_history(&harness.subject, 0, 10)
        .unwrap();
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].reason.as_deref(), Some("griefing"));
    assert_eq!(history[0].outcome, "succeeded");
    for command in [
        GameCommand::Console {
            expected_generation: generation,
            line: "stop".into(),
        },
        GameCommand::Moderate {
            expected_generation: generation,
            action: ModerationAction::Op {
                player: "Alex".into(),
            },
        },
        GameCommand::Moderate {
            expected_generation: generation,
            action: ModerationAction::Deop {
                player: "Alex".into(),
            },
        },
    ] {
        assert_eq!(
            handle
                .submit_game(&harness.subject, command, &uuid::Uuid::new_v4().to_string())
                .unwrap_err()
                .code(),
            "forbidden"
        );
    }
    harness
        .db
        .write(|c| {
            c.execute(
                "UPDATE role_assignments SET role_id='maintainer' WHERE principal_id=?",
                [harness.subject.principal.as_str()],
            )?;
            Ok(())
        })
        .unwrap();
    assert_eq!(
        handle
            .submit_game(
                &harness.subject,
                GameCommand::Moderate {
                    expected_generation: generation,
                    action: ModerationAction::Pardon {
                        player: "Alex".into()
                    }
                },
                "console-is-not-moderation"
            )
            .unwrap_err()
            .code(),
        "forbidden"
    );
    harness
        .db
        .write(|c| {
            c.execute(
                "UPDATE role_assignments SET role_id='administrator' WHERE principal_id=?",
                [harness.subject.principal.as_str()],
            )?;
            Ok(())
        })
        .unwrap();
    let admission = handle
        .submit_game(
            &harness.subject,
            GameCommand::Moderate {
                expected_generation: generation,
                action: ModerationAction::Op {
                    player: "Alex".into(),
                },
            },
            "confirmed-op",
        )
        .unwrap();
    let crate::operations::Admission::Created(op) = admission else {
        panic!("new op")
    };
    assert_eq!(
        op.state,
        crate::operations::OperationState::AwaitingConfirmation
    );
    assert_eq!(
        fs::read_to_string(harness.cwd.join("operation-commands")).unwrap(),
        "kick Alex griefing\n"
    );
    let receipt = handle
        .issue_confirmation(&harness.subject, &op.id, Duration::from_secs(120))
        .unwrap();
    handle
        .confirm(&harness.subject, &op.id, receipt.token)
        .unwrap();
    harness.wait_state(&op.id, crate::operations::OperationState::Succeeded);
    assert_eq!(
        fs::read_to_string(harness.cwd.join("operation-commands")).unwrap(),
        "kick Alex griefing\nop Alex\n"
    );
    let reason = "\u{1f600}".repeat(128);
    let crate::operations::Admission::Created(unicode) = handle
        .submit_game(
            &harness.subject,
            GameCommand::Moderate {
                expected_generation: generation,
                action: ModerationAction::Kick {
                    player: "abcdefghijklmnop".into(),
                    reason: Some(reason.clone()),
                },
            },
            "unicode-boundary",
        )
        .unwrap()
    else {
        panic!("new typed action")
    };
    harness.wait_state(&unicode.id, crate::operations::OperationState::Succeeded);
    assert!(
        fs::read_to_string(harness.cwd.join("operation-commands"))
            .unwrap()
            .ends_with(&format!("kick abcdefghijklmnop {reason}\n"))
    );
    assert_eq!(
        handle
            .submit_game(
                &harness.subject,
                GameCommand::Console {
                    expected_generation: generation,
                    line: "x".repeat(1025)
                },
                "console-bound-stays-fixed"
            )
            .unwrap_err()
            .code(),
        "invalid_request"
    );
}

#[test]
fn moderation_temporary_bans_expire_once_and_refuse_revoked_or_superseded_intent() {
    use crate::moderation::ModerationAction;
    let harness = Harness::new(false);
    let owned = harness.runtime(Duration::from_secs(1));
    let generation = start(&harness, &owned.runtime);
    let handle = owned.runtime.network_handle();
    let cell = Arc::new(std::sync::OnceLock::new());
    cell.set(handle.clone())
        .unwrap_or_else(|_| panic!("empty cell"));
    let files = crate::files::FileBrowser::open(
        harness.db.clone(),
        &harness.cwd,
        &[harness.db.directory().path()],
    )
    .unwrap();
    let actions = crate::automation_host::HostActions::new(harness.db.clone(), cell, None, files);
    let ban = |player: &str| {
        let crate::operations::Admission::Created(op) = handle
            .submit_game(
                &harness.subject,
                GameCommand::Moderate {
                    expected_generation: generation,
                    action: ModerationAction::Ban {
                        player: player.into(),
                        reason: Some("reason".into()),
                        duration_seconds: Some(60),
                    },
                },
                &uuid::Uuid::new_v4().to_string(),
            )
            .unwrap()
        else {
            panic!("new ban")
        };
        harness.wait_state(&op.id, crate::operations::OperationState::Succeeded);
        harness
            .db
            .write(|c| {
                c.execute(
                    "UPDATE moderation_actions SET expires_at=? WHERE operation_id=?",
                    params![harness.db.clock().millis(), op.id.as_str()],
                )?;
                Ok(())
            })
            .unwrap();
        op.id
    };
    let expiry_state = |id: &OperationId| {
        harness
            .db
            .read(|c| {
                Ok(c.query_row(
                    "SELECT expiry_state FROM moderation_actions WHERE operation_id=?",
                    [id.as_str()],
                    |r| r.get::<_, String>(0),
                )?)
            })
            .unwrap()
    };
    let write_bans = |name: &str, reason: &str| {
        fs::write(harness.cwd.join("banned-players.json"), serde_json::to_vec(&serde_json::json!([{"name":name,"uuid":"123e4567-e89b-42d3-a456-426614174000","reason":reason,"created":"today","expires":"forever","source":"Server"}])).unwrap()).unwrap()
    };
    let first = ban("Alex");
    write_bans("aLeX", "reason");
    actions.expire_bans(&harness.cwd).unwrap();
    wait_for(|| expiry_state(&first) == "completed");
    actions.expire_bans(&harness.cwd).unwrap();
    assert_eq!(
        fs::read_to_string(harness.cwd.join("operation-commands")).unwrap(),
        "ban Alex reason\npardon Alex\n"
    );
    let second = ban("Alex");
    write_bans("Alex", "changed externally");
    actions.expire_bans(&harness.cwd).unwrap();
    assert_eq!(expiry_state(&second), "superseded");
    let missing = ban("Absent");
    fs::remove_file(harness.cwd.join("banned-players.json")).unwrap();
    actions.expire_bans(&harness.cwd).unwrap();
    assert_eq!(expiry_state(&missing), "superseded");
    actions.expire_bans(&harness.cwd).unwrap();
    assert_eq!(harness.db.read(|c| Ok(c.query_row("SELECT count(*) FROM notifications WHERE title='Temporary ban expiry not confirmed'", [], |r| r.get::<_,i64>(0))?)).unwrap(), 2);
    let commands = fs::read_to_string(harness.cwd.join("operation-commands")).unwrap();
    assert!(!commands.lines().any(|v| v == "pardon Absent"));
    // A half-written ban file defers expiry; only persistent unreadability blocks it.
    let unreadable = ban("Robin");
    fs::write(harness.cwd.join("banned-players.json"), b"[{\"name\":").unwrap();
    actions.expire_bans(&harness.cwd).unwrap();
    assert_eq!(expiry_state(&unreadable), "pending");
    harness
        .db
        .write(|c| {
            c.execute(
                "UPDATE moderation_actions SET expires_at=? WHERE operation_id=?",
                params![
                    harness.db.clock().millis()
                        - crate::automation_host::UNREADABLE_BAN_FILE_GRACE_MS,
                    unreadable.as_str()
                ],
            )?;
            Ok(())
        })
        .unwrap();
    actions.expire_bans(&harness.cwd).unwrap();
    assert_eq!(expiry_state(&unreadable), "blocked");
    actions.expire_bans(&harness.cwd).unwrap();
    assert_eq!(harness.db.read(|c| Ok(c.query_row("SELECT count(*) FROM notifications WHERE title='Temporary ban expiry needs review'", [], |r| r.get::<_,i64>(0))?)).unwrap(), 1);
    let commands = fs::read_to_string(harness.cwd.join("operation-commands")).unwrap();
    assert!(!commands.lines().any(|v| v == "pardon Robin"));
    let third = ban("Steve");
    let crate::operations::Admission::Created(permanent) = handle
        .submit_game(
            &harness.subject,
            GameCommand::Moderate {
                expected_generation: generation,
                action: ModerationAction::Ban {
                    player: "sTeVe".into(),
                    reason: None,
                    duration_seconds: None,
                },
            },
            "permanent-ban",
        )
        .unwrap()
    else {
        panic!("new ban")
    };
    harness.wait_state(&permanent.id, crate::operations::OperationState::Succeeded);
    assert_eq!(expiry_state(&third), "superseded");
    let fourth = ban("Pat");
    harness.db.write(|c| { c.execute("INSERT INTO principal_overrides(id,principal_id,capability,scope,effect,granted_by,created_at) VALUES(?,?,'players.moderate','*','deny','fixture',?)", params![uuid::Uuid::new_v4().to_string(),harness.subject.principal.as_str(),harness.db.clock().millis()])?; Ok(()) }).unwrap();
    actions.expire_bans(&harness.cwd).unwrap();
    assert_eq!(expiry_state(&fourth), "blocked");
    assert_eq!(
        harness
            .db
            .read(|c| Ok(c.query_row(
                "SELECT count(*) FROM notifications WHERE title='Temporary ban expiry blocked'",
                [],
                |r| r.get::<_, i64>(0)
            )?))
            .unwrap(),
        1
    );
    actions.expire_bans(&harness.cwd).unwrap();
    assert_eq!(
        harness
            .db
            .read(|c| Ok(c.query_row(
                "SELECT count(*) FROM notifications WHERE title='Temporary ban expiry blocked'",
                [],
                |r| r.get::<_, i64>(0)
            )?))
            .unwrap(),
        1
    );
    harness
        .db
        .write(|c| {
            c.execute(
                "DELETE FROM principal_overrides WHERE capability='players.moderate'",
                [],
            )?;
            Ok(())
        })
        .unwrap();
    let offline = ban("Robin");
    let stop = harness.submit(GameCommand::Stop {
        expected_generation: generation,
    });
    harness.wait_state(&stop, crate::operations::OperationState::Succeeded);
    actions.expire_bans(&harness.cwd).unwrap();
    assert_eq!(expiry_state(&offline), "pending");
    start(&harness, &owned.runtime);
    write_bans("Robin", "reason");
    actions.expire_bans(&harness.cwd).unwrap();
    wait_for(|| expiry_state(&offline) == "completed");
    actions.expire_bans(&harness.cwd).unwrap();
    let commands = fs::read_to_string(harness.cwd.join("operation-commands")).unwrap();
    assert_eq!(commands.lines().filter(|v| *v == "pardon Robin").count(), 1);
}
