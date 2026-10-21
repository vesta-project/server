use super::{Fixture, key};
use crate::identity::{
    BuiltinRole, Capability,
    management::{Enrolled, ManagementError, SessionToken, permissions::*},
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use ring::signature::KeyPair;
use serde_json::{Value, json};

fn actor(f: &Fixture, role: BuiltinRole) -> (Enrolled, SessionToken) {
    let (d, k) = f.enroll(role);
    let s = f.session(&d, &k);
    (d, s)
}
fn get(f: &Fixture, s: &SessionToken, id: &str) -> Value {
    f.service
        .permission_query(s.secret.expose(), PermissionQuery::User(id.into()))
        .unwrap()
}
fn roles(add: &[&str], remove: &[&str], v: &Value) -> UserRoles {
    UserRoles {
        add: add.iter().map(|v| (*v).into()).collect(),
        remove: remove.iter().map(|v| (*v).into()).collect(),
        expected_version: v["version"].as_str().unwrap().into(),
    }
}
fn input(name: &str, rank: i32, caps: &[Capability]) -> RoleInput {
    RoleInput {
        name: name.into(),
        rank,
        capabilities: caps
            .iter()
            .map(|c| CapabilityInput {
                capability: c.id().into(),
                scope: "*".into(),
            })
            .collect(),
    }
}
fn run(
    f: &Fixture,
    s: &SessionToken,
    key: &str,
    c: PermissionMutation,
) -> Result<Value, PermissionError> {
    f.service.permission_mutate(s.secret.expose(), key, c)
}
fn create(f: &Fixture, s: &SessionToken, name: &str, rank: i32, caps: &[Capability]) -> Value {
    run(
        f,
        s,
        &format!("create-role-{}-0001", name.replace(" ", "-")),
        PermissionMutation::CreateRole(input(name, rank, caps)),
    )
    .unwrap()
}

#[test]
fn user_role_hierarchy_is_strict_in_both_directions_and_self_is_forbidden() {
    let f = Fixture::new();
    let (owner, os) = actor(&f, BuiltinRole::Owner);
    let (admin, as_) = actor(&f, BuiltinRole::Administrator);
    let (equal, _) = actor(&f, BuiltinRole::Administrator);
    let (lower, _) = actor(&f, BuiltinRole::Viewer);
    for (id, s) in [
        (&owner.principal_id, &as_),
        (&equal.principal_id, &as_),
        (&admin.principal_id, &as_),
        (&owner.principal_id, &os),
    ] {
        let v = get(&f, s, id);
        assert_eq!(
            run(
                &f,
                s,
                "rank-rule-retry-0001",
                PermissionMutation::UserRoles(id.clone(), roles(&["moderator"], &[], &v))
            ),
            Err(PermissionError::Forbidden)
        );
    }
    let v = get(&f, &as_, &lower.principal_id);
    let result = run(
        &f,
        &as_,
        "lower-role-add-0001",
        PermissionMutation::UserRoles(lower.principal_id.clone(), roles(&["moderator"], &[], &v)),
    )
    .unwrap();
    assert_eq!(result["rank"], 40);
    let result = run(
        &f,
        &as_,
        "lower-role-remove-0001",
        PermissionMutation::UserRoles(
            lower.principal_id.clone(),
            roles(&[], &["moderator"], &result),
        ),
    )
    .unwrap();
    assert_eq!(result["rank"], 20);
}
#[test]
fn superadmin_is_never_assignable_or_removable_remotely_even_by_another_owner() {
    let f = Fixture::new();
    let (_, os) = actor(&f, BuiltinRole::Owner);
    let (other, _) = actor(&f, BuiltinRole::Owner);
    let (target, _) = actor(&f, BuiltinRole::Viewer);
    for (id, add, remove) in [
        (&target.principal_id, vec!["owner"], vec![]),
        (&other.principal_id, vec![], vec!["owner"]),
        (&target.principal_id, vec![], vec!["owner"]),
    ] {
        let v = get(&f, &os, id);
        assert_eq!(
            run(
                &f,
                &os,
                "top-role-block-0001",
                PermissionMutation::UserRoles(id.clone(), roles(&add, &remove, &v))
            ),
            Err(PermissionError::Forbidden)
        );
    }
    for c in [
        PermissionMutation::CreateRole(input("Fake owner", 100, &[Capability::StatusView])),
        PermissionMutation::PatchRole(
            "owner".into(),
            RolePatch {
                expected_version: "0".repeat(64),
                name: Some("Changed".into()),
                rank: None,
                capabilities: None,
            },
        ),
        PermissionMutation::DeleteRole("owner".into(), "0".repeat(64)),
    ] {
        assert!(run(&f, &os, "builtin-refusal-0001", c).is_err());
    }
}
#[test]
fn role_creation_and_edit_are_bounded_by_rank_capabilities_and_live_dependents() {
    let f = Fixture::new();
    let (_, os) = actor(&f, BuiltinRole::Owner);
    let (_, as_) = actor(&f, BuiltinRole::Administrator);
    for i in [
        input("Too high", 80, &[Capability::StatusView]),
        input("Not held", 30, &[Capability::RecoveryExport]),
        input("Local only", 30, &[Capability::OwnerRecovery]),
    ] {
        assert!(
            run(
                &f,
                &as_,
                "create-denied-0001",
                PermissionMutation::CreateRole(i)
            )
            .is_err()
        );
    }
    let r = create(
        &f,
        &as_,
        "Lower manager",
        30,
        &[Capability::StatusView, Capability::RolesManage],
    );
    let id = r["id"].as_str().unwrap();
    for rank in [80, 100] {
        assert!(
            run(
                &f,
                &as_,
                "edit-denied-rank-0001",
                PermissionMutation::PatchRole(
                    id.into(),
                    RolePatch {
                        expected_version: r["version"].as_str().unwrap().into(),
                        name: None,
                        rank: Some(rank),
                        capabilities: None
                    }
                )
            )
            .is_err()
        );
    }
    assert_eq!(
        run(
            &f,
            &as_,
            "edit-denied-caps-0001",
            PermissionMutation::PatchRole(
                id.into(),
                RolePatch {
                    expected_version: r["version"].as_str().unwrap().into(),
                    name: None,
                    rank: None,
                    capabilities: Some(input("x", 30, &[Capability::RecoveryExport]).capabilities)
                }
            )
        ),
        Err(PermissionError::Forbidden)
    );
    let changed = run(
        &f,
        &as_,
        "edit-allowed-role-0001",
        PermissionMutation::PatchRole(
            id.into(),
            RolePatch {
                expected_version: r["version"].as_str().unwrap().into(),
                name: Some("Renamed lower manager".into()),
                rank: Some(31),
                capabilities: None,
            },
        ),
    )
    .unwrap();
    assert_eq!(changed["rank"], 31);
    let (me, _) = actor(&f, BuiltinRole::Owner);
    f.db.write_with(|tx|{tx.connection().execute("INSERT INTO role_assignments(id,principal_id,role_id,granted_by,created_at) VALUES('dependent-test',?,?, 'test',0)",rusqlite::params![me.principal_id,id])?;Ok::<_,rusqlite::Error>(())}).unwrap();
    assert_eq!(
        run(
            &f,
            &os,
            "edit-higher-dependent-0001",
            PermissionMutation::PatchRole(
                id.into(),
                RolePatch {
                    expected_version: changed["version"].as_str().unwrap().into(),
                    name: Some("Cannot alter owner".into()),
                    rank: None,
                    capabilities: None
                }
            )
        ),
        Err(PermissionError::Forbidden)
    );
}
#[test]
fn automatic_sources_refuse_management_and_edits_of_captured_defaults() {
    let f = Fixture::new();
    let (_, s) = actor(&f, BuiltinRole::Owner);
    let access = f
        .service
        .permission_query(s.secret.expose(), PermissionQuery::Access)
        .unwrap();
    let patch = AccessPatch {
        expected_version: access["version"].as_str().unwrap().into(),
        anonymous: Some("owner".into()),
        new_user: None,
        whitelist: None,
        whitelist_enabled: None,
        password: None,
    };
    assert_eq!(
        run(
            &f,
            &s,
            "unsafe-source-set-0001",
            PermissionMutation::Access(patch)
        ),
        Err(PermissionError::InvalidRequest)
    );
    let list = f
        .service
        .permission_query(s.secret.expose(), PermissionQuery::Roles)
        .unwrap();
    let public = list["roles"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == access["settings"]["anonymous"])
        .unwrap();
    assert!(
        run(
            &f,
            &s,
            "unsafe-source-edit-0001",
            PermissionMutation::PatchRole(
                public["id"].as_str().unwrap().into(),
                RolePatch {
                    expected_version: public["version"].as_str().unwrap().into(),
                    name: None,
                    rank: None,
                    capabilities: Some(input("unused", 1, &[Capability::RolesManage]).capabilities)
                }
            )
        )
        .is_err()
    );
}
#[test]
fn safe_access_patch_is_versioned_audited_and_idempotent() {
    let f = Fixture::new();
    let (_, s) = actor(&f, BuiltinRole::Owner);
    let v = f
        .service
        .permission_query(s.secret.expose(), PermissionQuery::Access)
        .unwrap();
    let c = PermissionMutation::Access(AccessPatch {
        expected_version: v["version"].as_str().unwrap().into(),
        anonymous: None,
        new_user: None,
        whitelist: None,
        whitelist_enabled: Some(false),
        password: None,
    });
    let first = run(&f, &s, "safe-access-patch-0001", c.clone()).unwrap();
    assert_eq!(first["settings"]["whitelist_enabled"], false);
    assert_eq!(
        run(&f, &s, "safe-access-patch-0001", c.clone()).unwrap(),
        first
    );
    assert_eq!(
        run(&f, &s, "stale-access-patch-0001", c),
        Err(PermissionError::PreconditionFailed)
    );
    assert!(f.service.audit.verify_chain().unwrap().valid);
}
#[test]
fn invitation_ceiling_secret_once_and_idempotent_receipt() {
    let f = Fixture::new();
    let (_, s) = actor(&f, BuiltinRole::Owner);
    assert_eq!(
        run(
            &f,
            &s,
            "invite-owner-refusal-0001",
            PermissionMutation::Invitation(Invitation {
                role: "owner".into(),
                expires_minutes: 10
            })
        ),
        Err(PermissionError::Forbidden)
    );
    let c = PermissionMutation::Invitation(Invitation {
        role: "viewer".into(),
        expires_minutes: 10,
    });
    let first = run(&f, &s, "invite-once-only-0001", c.clone()).unwrap();
    assert!(first["code"].is_string());
    let retry = run(&f, &s, "invite-once-only-0001", c).unwrap();
    assert!(retry.get("code").is_none());
    assert_eq!(retry["expires_at_ms"], first["expires_at_ms"]);
    let k = key();
    let enrolled = f
        .service
        .enroll(
            first["code"].as_str().unwrap(),
            "Invited",
            &URL_SAFE_NO_PAD.encode(k.public_key().as_ref()),
        )
        .unwrap();
    assert_eq!(enrolled.role, "viewer");
    assert_eq!(
        f.service
            .enroll(
                first["code"].as_str().unwrap(),
                "Replay",
                &URL_SAFE_NO_PAD.encode(k.public_key().as_ref())
            )
            .unwrap_err(),
        ManagementError::PairInvalid
    );
    f.db.read(|c| {
        let receipt: String = c.query_row(
            "SELECT result_json FROM management_permission_receipts",
            [],
            |r| r.get(0),
        )?;
        assert!(!receipt.contains(first["code"].as_str().unwrap()));
        Ok(())
    })
    .unwrap();
}
#[test]
fn invitation_rechecks_issuer_rank_and_capabilities_at_redemption() {
    let f = Fixture::new();
    let (issuer, s) = actor(&f, BuiltinRole::Administrator);
    let invited = create(&f, &s, "Invitation role", 30, &[Capability::StatusView]);
    let first = run(
        &f,
        &s,
        "invite-live-ceiling-0001",
        PermissionMutation::Invitation(Invitation {
            role: invited["id"].as_str().unwrap().into(),
            expires_minutes: 10,
        }),
    )
    .unwrap();
    f.db.write_with(|tx| {
        tx.connection().execute(
            "UPDATE role_assignments SET revoked_at=1 WHERE principal_id=?",
            [issuer.principal_id],
        )?;
        Ok::<_, rusqlite::Error>(())
    })
    .unwrap();
    let k = key();
    assert_eq!(
        f.service
            .enroll(
                first["code"].as_str().unwrap(),
                "Invited",
                &URL_SAFE_NO_PAD.encode(k.public_key().as_ref())
            )
            .unwrap_err(),
        ManagementError::Forbidden
    );
}
#[test]
fn invitation_cannot_attach_to_issuer_or_equal_rank_user() {
    let f = Fixture::new();
    let (issuer, s) = actor(&f, BuiltinRole::Owner);
    let (equal, es) = actor(&f, BuiltinRole::Owner);
    for (target, token) in [(issuer, s), (equal, es)] {
        let invite = run(
            &f,
            &token,
            "invite-attach-check-0001",
            PermissionMutation::Invitation(Invitation {
                role: "viewer".into(),
                expires_minutes: 10,
            }),
        )
        .unwrap();
        let k = key();
        assert_eq!(
            f.service
                .enroll_for(
                    invite["code"].as_str().unwrap(),
                    "Attach",
                    &URL_SAFE_NO_PAD.encode(k.public_key().as_ref()),
                    Some(token.secret.expose())
                )
                .unwrap_err(),
            ManagementError::Forbidden
        );
        assert_eq!(get(&f, &token, &target.principal_id)["rank"], 100);
    }
}
#[test]
fn device_revoke_scope_sessions_and_idempotent_retry() {
    let f = Fixture::new();
    let (owner, os) = actor(&f, BuiltinRole::Owner);
    let (admin, as_) = actor(&f, BuiltinRole::Administrator);
    let (equal, _) = actor(&f, BuiltinRole::Administrator);
    let (lower, ls) = actor(&f, BuiltinRole::Viewer);
    for id in [&owner.device_id, &equal.device_id] {
        assert_eq!(
            run(
                &f,
                &as_,
                "device-hierarchy-0001",
                PermissionMutation::RevokeDevice(id.clone())
            ),
            Err(PermissionError::Forbidden)
        );
    }
    let other_owner = actor(&f, BuiltinRole::Owner).0;
    assert_eq!(
        run(
            &f,
            &os,
            "owner-device-hierarchy-0001",
            PermissionMutation::RevokeDevice(other_owner.device_id)
        ),
        Err(PermissionError::Forbidden)
    );
    let c = PermissionMutation::RevokeDevice(lower.device_id.clone());
    assert_eq!(
        run(&f, &as_, "device-revoke-lower-0001", c.clone()).unwrap(),
        json!({"revoked":true})
    );
    assert!(f.service.authenticate(ls.secret.expose()).is_err());
    assert!(run(&f, &as_, "device-revoke-lower-0001", c).is_ok());
    assert!(
        run(
            &f,
            &as_,
            "device-revoke-self-0001",
            PermissionMutation::RevokeDevice(admin.device_id)
        )
        .is_ok()
    );
    assert!(f.service.authenticate(as_.secret.expose()).is_err());
}
#[test]
fn local_superadmin_grant_revoke_preserves_roles_and_last_owner() {
    let f = Fixture::new();
    let (owner, s) = actor(&f, BuiltinRole::Owner);
    let (target, _) = actor(&f, BuiltinRole::Viewer);
    assert_eq!(
        f.service.superadmin_local(&owner.principal_id, false),
        Err(PermissionError::Conflict)
    );
    f.service
        .superadmin_local(&target.principal_id, true)
        .unwrap();
    let v = get(&f, &s, &target.principal_id);
    assert_eq!(v["rank"], 100);
    assert_eq!(v["roles"].as_array().unwrap().len(), 2);
    f.service
        .superadmin_local(&owner.principal_id, false)
        .unwrap();
    assert_eq!(
        f.service.superadmin_local(&target.principal_id, false),
        Err(PermissionError::Conflict)
    );
    assert!(f.service.audit.verify_chain().unwrap().valid);
}
#[test]
fn mutations_roll_back_policy_state_receipt_and_success_when_audit_fails() {
    let f = Fixture::new();
    let (_, s) = actor(&f, BuiltinRole::Owner);
    let (target, _) = actor(&f, BuiltinRole::Viewer);
    let before = get(&f, &s, &target.principal_id);
    f.db.write_with(|tx|{tx.connection().execute_batch("CREATE TRIGGER fail_permission_audit BEFORE INSERT ON audit_events BEGIN SELECT RAISE(ABORT,'test'); END;")?;Ok::<_,rusqlite::Error>(())}).unwrap();
    assert_eq!(
        run(
            &f,
            &s,
            "atomic-audit-failure-0001",
            PermissionMutation::UserRoles(
                target.principal_id.clone(),
                roles(&["moderator"], &[], &before)
            )
        ),
        Err(PermissionError::Unavailable)
    );
    assert_eq!(get(&f, &s, &target.principal_id), before);
    f.db.read(|c| {
        let count: i64 = c.query_row(
            "SELECT count(*) FROM management_permission_receipts",
            [],
            |r| r.get(0),
        )?;
        assert_eq!(count, 0);
        Ok(())
    })
    .unwrap();
}
#[test]
fn role_and_user_versions_idempotency_conflict_and_delete() {
    let f = Fixture::new();
    let (_, s) = actor(&f, BuiltinRole::Owner);
    let (target, _) = actor(&f, BuiltinRole::Viewer);
    let v = get(&f, &s, &target.principal_id);
    let c =
        PermissionMutation::UserRoles(target.principal_id.clone(), roles(&["moderator"], &[], &v));
    let first = run(&f, &s, "user-retry-same-0001", c.clone()).unwrap();
    assert_eq!(
        run(&f, &s, "user-retry-same-0001", c.clone()).unwrap(),
        first
    );
    assert_eq!(
        run(&f, &s, "user-stale-new-key-0001", c),
        Err(PermissionError::PreconditionFailed)
    );
    assert_eq!(
        run(
            &f,
            &s,
            "user-retry-same-0001",
            PermissionMutation::UserRoles(
                target.principal_id.clone(),
                roles(&[], &["moderator"], &first)
            )
        ),
        Err(PermissionError::IdempotencyConflict)
    );
    let r = create(&f, &s, "Disposable role", 15, &[Capability::StatusView]);
    let c = PermissionMutation::DeleteRole(
        r["id"].as_str().unwrap().into(),
        r["version"].as_str().unwrap().into(),
    );
    assert!(run(&f, &s, "delete-role-retry-0001", c.clone()).is_ok());
    assert!(run(&f, &s, "delete-role-retry-0001", c).is_ok());
}
#[test]
fn users_pagination_devices_and_specific_capabilities() {
    let f = Fixture::new();
    let (owner, s) = actor(&f, BuiltinRole::Owner);
    let (viewer, vs) = actor(&f, BuiltinRole::Viewer);
    let page = f
        .service
        .permission_query(
            s.secret.expose(),
            PermissionQuery::Users {
                offset: 0,
                limit: 1,
            },
        )
        .unwrap();
    assert_eq!(page["users"].as_array().unwrap().len(), 1);
    assert_eq!(page["next_offset"], 1);
    assert_eq!(
        f.service.permission_query(
            vs.secret.expose(),
            PermissionQuery::User(owner.principal_id)
        ),
        Err(PermissionError::Forbidden)
    );
    assert_eq!(
        f.service
            .permission_query(
                s.secret.expose(),
                PermissionQuery::Devices(viewer.principal_id)
            )
            .unwrap()["devices"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}
#[test]
fn invitations_are_bounded() {
    let f = Fixture::new();
    let (_, s) = actor(&f, BuiltinRole::Owner);
    for i in 0..super::super::MAX_CODES {
        assert!(
            run(
                &f,
                &s,
                &format!("bounded-invitation-{i:04}"),
                PermissionMutation::Invitation(Invitation {
                    role: "viewer".into(),
                    expires_minutes: 10
                })
            )
            .is_ok()
        );
    }
    assert_eq!(
        run(
            &f,
            &s,
            "bounded-invitation-full",
            PermissionMutation::Invitation(Invitation {
                role: "viewer".into(),
                expires_minutes: 10
            })
        ),
        Err(PermissionError::Saturated)
    );
}

fn fill_receipts(f: &Fixture, completed_at: i64) {
    f.db.write_with(|tx| {
        let c = tx.connection();
        let mut insert = c.prepare(
            "INSERT INTO management_permission_receipts(key_hash,intent_hash,result_json,completed_at) VALUES(?,?,'{}',?)",
        )?;
        for i in 0u32..4096 {
            let mut key = [0u8; 32];
            key[..4].copy_from_slice(&i.to_be_bytes());
            insert.execute(rusqlite::params![key.as_slice(), [7u8; 32].as_slice(), completed_at])?;
        }
        Ok::<_, rusqlite::Error>(())
    })
    .unwrap();
}
fn receipt_count(f: &Fixture) -> i64 {
    f.db.read(|c| {
        Ok(c.query_row(
            "SELECT count(*) FROM management_permission_receipts",
            [],
            |r| r.get(0),
        )?)
    })
    .unwrap()
}

#[test]
fn expired_receipts_are_purged_so_a_full_store_never_blocks_changes_forever() {
    let f = Fixture::new();
    let (_, s) = actor(&f, BuiltinRole::Owner);
    let (target, _) = actor(&f, BuiltinRole::Viewer);
    fill_receipts(
        &f,
        f.clock.0.load(std::sync::atomic::Ordering::SeqCst) - 8 * 24 * 60 * 60 * 1000,
    );
    assert_eq!(receipt_count(&f), 4096);
    let before = get(&f, &s, &target.principal_id);
    run(
        &f,
        &s,
        "after-expired-receipts-0001",
        PermissionMutation::UserRoles(
            target.principal_id.clone(),
            roles(&["moderator"], &[], &before),
        ),
    )
    .unwrap();
    // One bounded batch was purged and the new receipt was stored.
    assert_eq!(receipt_count(&f), 4096 - 128 + 1);
}

#[test]
fn recent_receipts_still_cap_the_store_and_retries_still_replay() {
    let f = Fixture::new();
    let (_, s) = actor(&f, BuiltinRole::Owner);
    let (target, _) = actor(&f, BuiltinRole::Viewer);
    let before = get(&f, &s, &target.principal_id);
    let change = || {
        PermissionMutation::UserRoles(
            target.principal_id.clone(),
            roles(&["moderator"], &[], &before),
        )
    };
    let first = run(&f, &s, "replayed-within-window-0001", change()).unwrap();
    // A retry with the same key replays the stored result instead of reapplying.
    assert_eq!(
        run(&f, &s, "replayed-within-window-0001", change()).unwrap(),
        first
    );
    // Six-day-old receipts are still inside the retention window and are not purged.
    let six_days = 6 * 24 * 60 * 60 * 1000;
    fill_receipts(
        &f,
        f.clock.0.load(std::sync::atomic::Ordering::SeqCst) - six_days,
    );
    let current = get(&f, &s, &target.principal_id);
    assert_eq!(
        run(
            &f,
            &s,
            "store-full-of-recent-0001",
            PermissionMutation::UserRoles(
                target.principal_id.clone(),
                roles(&[], &["moderator"], &current)
            ),
        ),
        Err(PermissionError::Saturated)
    );
}

fn assign_only(f: &Fixture, principal: &str, role: &str) {
    f.db.write_with(|tx| {
        tx.connection().execute("UPDATE role_assignments SET revoked_at=1 WHERE principal_id=?", [principal])?;
        tx.connection().execute("INSERT INTO role_assignments(id,principal_id,role_id,granted_by,created_at) VALUES(?,?,?,'test',0)", rusqlite::params![format!("ceiling-{principal}"), principal, role])?;
        Ok::<_, rusqlite::Error>(())
    }).unwrap();
}

fn revoke_override(f: &Fixture, principal: &str, expires: Option<i64>, revoked: Option<i64>) {
    f.db.write_with(|tx| {
        tx.connection().execute("INSERT INTO principal_overrides(id,principal_id,capability,scope,effect,granted_by,created_at,expires_at,revoked_at) VALUES('ceiling-override',?,'devices.revoke','*','allow','test',0,?,?)", rusqlite::params![principal, expires, revoked])?;
        Ok::<_, rusqlite::Error>(())
    }).unwrap();
}

#[test]
fn invitation_redemption_checks_active_allow_overrides_and_rolls_back() {
    for (expires, revoked, allowed) in [
        (None, None, false),
        (Some(1), None, true),
        (None, Some(1), true),
    ] {
        let f = Fixture::new();
        let (_, owner) = actor(&f, BuiltinRole::Owner);
        let manager = create(
            &f,
            &owner,
            "Invitation ceiling manager",
            60,
            &[Capability::InvitationsCreate, Capability::StatusView],
        );
        let low = create(&f, &owner, "Invitation low", 10, &[Capability::StatusView]);
        let high = create(&f, &owner, "Invitation high", 50, &[Capability::StatusView]);
        let (issuer, issuer_session) = actor(&f, BuiltinRole::Viewer);
        let (target, target_session) = actor(&f, BuiltinRole::Viewer);
        assign_only(&f, &issuer.principal_id, manager["id"].as_str().unwrap());
        assign_only(&f, &target.principal_id, low["id"].as_str().unwrap());
        revoke_override(&f, &target.principal_id, expires, revoked);
        let invite = run(
            &f,
            &issuer_session,
            "override-invitation-0001",
            PermissionMutation::Invitation(Invitation {
                role: high["id"].as_str().unwrap().into(),
                expires_minutes: 10,
            }),
        )
        .unwrap();
        let before = get(&f, &owner, &target.principal_id);
        let k = key();
        let result = f.service.enroll_for(
            invite["code"].as_str().unwrap(),
            "Attach",
            &URL_SAFE_NO_PAD.encode(k.public_key().as_ref()),
            Some(target_session.secret.expose()),
        );
        if allowed {
            result.unwrap();
            assert_eq!(get(&f, &owner, &target.principal_id)["rank"], 50);
        } else {
            assert_eq!(result.unwrap_err(), ManagementError::Forbidden);
            assert_eq!(get(&f, &owner, &target.principal_id), before);
        }
    }
}

#[test]
fn role_rank_increase_checks_dependents_overrides_and_other_assigned_roles() {
    for via_role in [false, true] {
        let f = Fixture::new();
        let (_, owner) = actor(&f, BuiltinRole::Owner);
        let manager = create(
            &f,
            &owner,
            "Rank ceiling manager",
            60,
            &[Capability::RolesManage, Capability::StatusView],
        );
        let low = create(&f, &owner, "Rank low", 10, &[Capability::StatusView]);
        let (issuer, issuer_session) = actor(&f, BuiltinRole::Viewer);
        let (target, _) = actor(&f, BuiltinRole::Viewer);
        assign_only(&f, &issuer.principal_id, manager["id"].as_str().unwrap());
        assign_only(&f, &target.principal_id, low["id"].as_str().unwrap());
        if via_role {
            let extra = create(
                &f,
                &owner,
                "Existing revoker",
                9,
                &[Capability::DevicesRevoke],
            );
            f.db.write_with(|tx| {
                tx.connection().execute("INSERT INTO role_assignments(id,principal_id,role_id,granted_by,created_at) VALUES('existing-revoker',?,?,'test',0)", rusqlite::params![target.principal_id, extra["id"].as_str().unwrap()])?;
                Ok::<_, rusqlite::Error>(())
            }).unwrap();
        } else {
            revoke_override(&f, &target.principal_id, None, None);
        }
        let before = get(&f, &owner, &target.principal_id);
        let patch = |rank| {
            PermissionMutation::PatchRole(
                low["id"].as_str().unwrap().into(),
                RolePatch {
                    expected_version: low["version"].as_str().unwrap().into(),
                    name: None,
                    rank: Some(rank),
                    capabilities: None,
                },
            )
        };
        assert_eq!(
            run(&f, &issuer_session, "rank-increase-ceiling-0001", patch(50)),
            Err(PermissionError::Forbidden)
        );
        assert_eq!(get(&f, &owner, &target.principal_id), before);
        // Rank reductions do not amplify permissions and remain permitted.
        assert_eq!(
            run(&f, &issuer_session, "rank-decrease-ceiling-0001", patch(8)).unwrap()["rank"],
            8
        );
    }
}

#[test]
fn branding_audit_failure_rolls_back_setting_and_receipt() {
    let f = Fixture::new();
    let (_, session) = actor(&f, BuiltinRole::Owner);
    f.db.write(|c| {c.execute_batch("CREATE TRIGGER fail_branding_audit BEFORE INSERT ON audit_events WHEN NEW.action='management.permissions.mutate' BEGIN SELECT RAISE(ABORT,'fixture'); END;")?;Ok(())}).unwrap();
    let command = PermissionMutation::Server(branding::ServerPatch {
        expected_revision: "0".into(),
        name: Some("Brand".into()),
        description: None,
    });
    assert_eq!(
        run(&f, &session, "branding-rollback-00001", command.clone()),
        Err(PermissionError::Unavailable)
    );
    assert_eq!(
        f.service
            .permission_query(session.secret.expose(), PermissionQuery::Server)
            .unwrap()["revision"],
        "0"
    );
    f.db.write(|c| {
        c.execute_batch("DROP TRIGGER fail_branding_audit;")?;
        Ok(())
    })
    .unwrap();
    assert_eq!(
        run(&f, &session, "branding-rollback-00001", command).unwrap()["revision"],
        "1"
    );
}
