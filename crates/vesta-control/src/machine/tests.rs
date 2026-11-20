use super::{
    HostState,
    owners::{Actor, EffectiveRole, HostRole, ServerRole},
    registry::{ServerEntry, ServerState, Visibility},
};
use std::sync::{Arc, Barrier};
use uuid::Uuid;
use vesta_state::SystemClock;

fn open(path: &std::path::Path) -> HostState {
    HostState::open(path, Arc::new(SystemClock)).unwrap()
}
fn enroll(state: &HostState, account: Uuid, key: u8) -> super::owners::Device {
    state
        .write(|c| HostState::enroll_device_in(c, account, "Laptop", [key; 32], state.now_ms()))
        .unwrap()
}
fn register(state: &HostState, owner: Uuid, port: u16) -> ServerEntry {
    let server = ServerEntry {
        server_id: Uuid::new_v4(),
        name: "Survival".into(),
        game_port: port,
        state: ServerState::Stopped,
        visibility: Visibility::Private,
    };
    state
        .register_server(
            Actor::Account(owner),
            &server,
            state.snapshot().unwrap().revision,
        )
        .unwrap();
    server
}
#[test]
fn machine_concurrent_claims_have_exactly_one_primary() {
    let dir = tempfile::tempdir().unwrap();
    let state = open(dir.path());
    let gate = Arc::new(Barrier::new(2));
    // Independent database handles exercise SQLite transaction admission as well
    // as cloned handles' FIFO gate. Both are initialized before the race.
    let other = open(dir.path());
    let threads = [state.clone(), other]
        .into_iter()
        .enumerate()
        .map(|(i, state)| {
            let gate = gate.clone();
            std::thread::spawn(move || {
                gate.wait();
                let id = Uuid::new_v4();
                (id, state.claim(id, "Laptop", [i as u8 + 1; 32]))
            })
        })
        .collect::<Vec<_>>();
    let results = threads
        .into_iter()
        .map(|t| t.join().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(results.iter().filter(|(_, r)| r.is_ok()).count(), 1);
    let winner = results.iter().find(|(_, r)| r.is_ok()).unwrap().0;
    assert_eq!(state.snapshot().unwrap().primary_account_id, Some(winner));
    state
        .read(|c| {
            assert_eq!(
                c.query_row("SELECT count(*) FROM machine_accounts", [], |r| r
                    .get::<_, i64>(0))?,
                1
            );
            Ok(())
        })
        .unwrap();
}
#[test]
fn machine_owners_see_later_servers_and_grants_do_not_leak() {
    let dir = tempfile::tempdir().unwrap();
    let state = open(dir.path());
    let primary = Uuid::new_v4();
    let owner = Uuid::new_v4();
    let staff = Uuid::new_v4();
    state.claim(primary, "Primary", [1; 32]).unwrap();
    enroll(&state, owner, 2);
    enroll(&state, staff, 3);
    state
        .set_owner(
            Actor::Account(primary),
            owner,
            true,
            state.snapshot().unwrap().revision,
        )
        .unwrap();
    assert!(state.visible_servers(owner).unwrap().is_empty());
    let mut first = register(&state, primary, 25565);
    assert_eq!(
        state.visible_servers(owner).unwrap()[0].1,
        EffectiveRole::Owner
    );
    state
        .set_server_role(
            Actor::Account(owner),
            first.server_id,
            staff,
            Some(ServerRole::Admin),
            state.snapshot().unwrap().revision,
        )
        .unwrap();
    assert_eq!(
        state.effective_role(staff, first.server_id).unwrap(),
        Some(EffectiveRole::Server(ServerRole::Admin))
    );
    let blocked = ServerEntry {
        server_id: Uuid::new_v4(),
        ..first.clone()
    };
    assert!(
        state
            .register_server(
                Actor::Account(owner),
                &blocked,
                state.snapshot().unwrap().revision
            )
            .is_err()
    );
    first.state = ServerState::Deleted;
    state
        .update_server(
            Actor::Account(owner),
            &first,
            state.snapshot().unwrap().revision,
        )
        .unwrap();
    let second = register(&state, primary, 25565);
    assert_eq!(state.effective_role(staff, second.server_id).unwrap(), None);
    assert!(state.visible_servers(staff).unwrap().is_empty());
    assert_eq!(
        state.visible_servers(owner).unwrap()[0].0.server_id,
        second.server_id
    );
    assert!(
        state
            .register_server(
                Actor::Account(owner),
                &first,
                state.snapshot().unwrap().revision
            )
            .is_err()
    );
    // A two-server policy fixture exercises the future capacity seam directly.
    state
        .write(|c| {
            c.execute(
                "UPDATE machine_servers SET state='stopped',game_port=25566 WHERE server_id=?1",
                [first.server_id.to_string()],
            )?;
            Ok(())
        })
        .unwrap();
    assert_eq!(state.visible_servers(staff).unwrap().len(), 1);
    assert_eq!(state.effective_role(staff, second.server_id).unwrap(), None);
    assert!(
        state
            .set_server_role(
                Actor::Account(staff),
                second.server_id,
                owner,
                Some(ServerRole::Admin),
                state.snapshot().unwrap().revision
            )
            .is_err()
    );
}
#[test]
fn machine_primary_protection_transfer_and_revocation_generation() {
    let dir = tempfile::tempdir().unwrap();
    let state = open(dir.path());
    let primary = Uuid::new_v4();
    let owner = Uuid::new_v4();
    let device = state.claim(primary, "Primary", [1; 32]).unwrap();
    enroll(&state, owner, 2);
    state
        .set_owner(
            Actor::Account(primary),
            owner,
            true,
            state.snapshot().unwrap().revision,
        )
        .unwrap();
    let before = state.snapshot().unwrap();
    assert!(
        state
            .set_owner(Actor::Account(owner), primary, false, before.revision)
            .is_err()
    );
    assert!(
        state
            .set_owner(Actor::Account(primary), primary, false, before.revision)
            .is_err()
    );
    assert!(
        state
            .revoke_device(Actor::Account(owner), device.device_id)
            .is_err()
    );
    assert!(
        state
            .transfer_ownership(Actor::Account(owner), primary, before.revision)
            .is_err()
    );
    assert_eq!(state.snapshot().unwrap(), before);
    let after = state
        .revoke_device(Actor::Account(primary), device.device_id)
        .unwrap();
    assert_eq!(after.auth_generation, before.auth_generation + 1);
    assert!(state.active_device(device.device_id).unwrap().is_none());
    state
        .transfer_ownership(Actor::Account(primary), owner, after.revision)
        .unwrap();
    assert_eq!(
        state.host_role(owner).unwrap(),
        Some(HostRole::PrimaryOwner)
    );
    assert_eq!(state.host_role(primary).unwrap(), Some(HostRole::Owner));
    let new_device = enroll(&state, owner, 3);
    assert!(
        state
            .revoke_device(Actor::Account(primary), new_device.device_id)
            .is_err()
    );
    state
        .revoke_device(Actor::LocalRecovery, new_device.device_id)
        .unwrap();
}
#[test]
fn machine_restart_persists_identity_registry_grants_devices_and_counters() {
    let dir = tempfile::tempdir().unwrap();
    let state = open(dir.path());
    let primary = Uuid::new_v4();
    let staff = Uuid::new_v4();
    let device = state.claim(primary, "Primary", [1; 32]).unwrap();
    enroll(&state, staff, 2);
    let server = register(&state, primary, 25565);
    assert_ne!(state.snapshot().unwrap().host_id, server.server_id);
    assert_ne!(
        state.snapshot().unwrap().host_id.to_string(),
        state.database().server_id()
    );
    state
        .set_server_role(
            Actor::Account(primary),
            server.server_id,
            staff,
            Some(ServerRole::Moderator),
            state.snapshot().unwrap().revision,
        )
        .unwrap();
    state
        .revoke_device(Actor::Account(primary), device.device_id)
        .unwrap();
    let snapshot = state.snapshot().unwrap();
    state.database().close().unwrap();
    drop(state);
    let reopened = open(dir.path());
    assert_eq!(reopened.snapshot().unwrap(), snapshot);
    assert_eq!(reopened.server(server.server_id).unwrap(), Some(server));
    assert_eq!(
        reopened
            .effective_role(
                staff,
                reopened.visible_servers(staff).unwrap()[0].0.server_id
            )
            .unwrap(),
        Some(EffectiveRole::Server(ServerRole::Moderator))
    );
    assert!(reopened.active_device(device.device_id).unwrap().is_none());
    assert!(reopened.claim(Uuid::new_v4(), "Again", [5; 32]).is_err());
}
#[test]
fn machine_stale_revision_and_key_rebinding_roll_back() {
    let dir = tempfile::tempdir().unwrap();
    let state = open(dir.path());
    let primary = Uuid::new_v4();
    let other = Uuid::new_v4();
    state.claim(primary, "Primary", [1; 32]).unwrap();
    let before = state.snapshot().unwrap();
    assert!(
        state
            .write(|c| HostState::enroll_device_in(c, other, "Duplicate", [1; 32], state.now_ms()))
            .is_err()
    );
    assert_eq!(state.snapshot().unwrap(), before);
    enroll(&state, other, 2);
    assert!(
        state
            .set_owner(Actor::Account(primary), other, true, before.revision)
            .is_err()
    );
    assert_eq!(state.host_role(other).unwrap(), None);
}

#[test]
fn machine_counter_exhaustion_and_failed_enrollment_are_atomic() {
    let dir = tempfile::tempdir().unwrap();
    let state = open(dir.path());
    let primary = Uuid::new_v4();
    let device = state.claim(primary, "Primary", [1; 32]).unwrap();
    state
        .write(|c| {
            c.execute("UPDATE machine_host SET auth_generation=?1", [i64::MAX])?;
            Ok(())
        })
        .unwrap();
    let before = state.snapshot().unwrap();
    assert!(
        state
            .revoke_device(Actor::Account(primary), device.device_id)
            .is_err()
    );
    assert!(state.active_device(device.device_id).unwrap().is_some());
    assert_eq!(state.snapshot().unwrap(), before);
    let other = Uuid::new_v4();
    assert!(
        state
            .write(|c| HostState::enroll_device_in(c, other, "Other", [2; 32], state.now_ms()))
            .is_err()
    );
    assert_eq!(state.devices(Actor::Account(primary)).unwrap().len(), 1);
    assert_eq!(state.snapshot().unwrap(), before);
    state
        .read(|c| {
            assert_eq!(
                c.query_row(
                    "SELECT count(*) FROM machine_accounts WHERE account_id=?1",
                    [other.to_string()],
                    |r| r.get::<_, i64>(0)
                )?,
                0
            );
            Ok(())
        })
        .unwrap();
}

#[test]
fn removed_servers_revoke_grants_replay_restore_and_expire() {
    use super::removed::{self, RemoveRequest};
    let temp = tempfile::tempdir().unwrap();
    let state = open(&temp.path().join("host"));
    let owner = Uuid::new_v4();
    state.claim(owner, "Owner", [1; 32]).unwrap();
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let server = register(&state, owner, port);
    let player = Uuid::new_v4();
    enroll(&state, player, 2);
    state
        .set_server_role(
            Actor::Account(owner),
            server.server_id,
            player,
            Some(ServerRole::Player),
            state.snapshot().unwrap().revision,
        )
        .unwrap();
    let servers = temp.path().join("servers");
    std::fs::create_dir_all(servers.join(server.server_id.to_string())).unwrap();
    let bad = RemoveRequest {
        confirm_name: "wrong".into(),
        idempotency_key: Uuid::new_v4(),
        permanently: false,
        keep_backups: None,
    };
    assert_eq!(
        removed::prepare(&state, owner, server.server_id, &bad)
            .err()
            .unwrap()
            .0,
        "confirm_name_mismatch"
    );
    assert_eq!(
        removed::prepare(&state, player, server.server_id, &bad)
            .err()
            .unwrap()
            .0,
        "forbidden"
    );
    let req = RemoveRequest {
        confirm_name: server.name.clone(),
        idempotency_key: Uuid::new_v4(),
        permanently: false,
        keep_backups: None,
    };
    let (record, done) = removed::prepare(&state, owner, server.server_id, &req).unwrap();
    assert!(!done);
    let mut renamed=server.clone();renamed.name="Renamed".into();
    state.update_server(Actor::Account(owner),&renamed,state.snapshot().unwrap().revision).unwrap();
    assert_eq!(removed::prepare(&state,owner,server.server_id,&req).err().unwrap().0,"confirm_name_mismatch");
    state.update_server(Actor::Account(owner),&server,state.snapshot().unwrap().revision).unwrap();
    state.write(|c| {c.execute_batch("CREATE TABLE machine_import_jobs(server_id TEXT,state TEXT)")?;c.execute("INSERT INTO machine_import_jobs VALUES(?,'applying')",[server.server_id.to_string()])?;Ok(())}).unwrap();
    assert_eq!(removed::prepare(&state,owner,server.server_id,&req).err().unwrap().0,"server_busy");
    state.write(|c| {c.execute("DELETE FROM machine_import_jobs",[])?;Ok(())}).unwrap();
    removed::finish(&state, &servers, &record, &req).unwrap();
    assert!(state.visible_servers(owner).unwrap().is_empty());
    assert!(
        state
            .effective_role(player, server.server_id)
            .unwrap()
            .is_none()
    );
    let (replay, done) = removed::prepare(&state, owner, server.server_id, &req).unwrap();
    assert!(done);
    assert_eq!(record.removed_at, replay.removed_at);
    let root = temp.path().join("removed");
    assert_eq!(removed::list(&root).unwrap().len(), 1);
    removed::restore(&state, &servers, server.server_id).unwrap();
    assert_eq!(state.visible_servers(owner).unwrap().len(), 1);
    assert!(
        state
            .effective_role(player, server.server_id)
            .unwrap()
            .is_none()
    );
    let req = RemoveRequest {
        confirm_name: server.name,
        idempotency_key: Uuid::new_v4(),
        permanently: false,
        keep_backups: None,
    };
    let (record, _) = removed::prepare(&state, owner, server.server_id, &req).unwrap();
    removed::finish(&state, &servers, &record, &req).unwrap();
    removed::purge_expired(&state, &servers, record.restore_until_ms + 1).unwrap();
    assert!(removed::list(&root).unwrap().is_empty());
}
#[test]
fn host_device_counts_exclude_revoked_devices() {
    let temp = tempfile::tempdir().unwrap();
    let state = open(temp.path());
    let owner = Uuid::new_v4();
    let device = state.claim(owner, "Owner", [1; 32]).unwrap();
    assert_eq!(state.active_device_counts().unwrap()[&owner.to_string()], 1);
    state
        .write(|c| {
            c.execute(
                "UPDATE machine_devices SET revoked_at_ms=1 WHERE account_id=?",
                [owner.to_string()],
            )?;
            Ok(())
        })
        .unwrap();
    assert!(
        !state
            .active_device_counts()
            .unwrap()
            .contains_key(&owner.to_string())
    );
    let _ = device;
}
