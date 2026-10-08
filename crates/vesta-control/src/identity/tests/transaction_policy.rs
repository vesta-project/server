//! The operations seam borrows current policy from the supplied unit of work.
use super::support::{Fixture, INITIAL_TIME_MS};
use crate::identity::{BuiltinRole, Capability, Decision, DenyReason, Effect, RoleId, policy_in};
use rusqlite::params;

#[test]
fn policy_in_observes_the_supplied_writers_uncommitted_revocation() {
    let fixture = Fixture::open();
    let subject = fixture.subject(RoleId::from(BuiltinRole::Viewer));
    fixture
        .db
        .write_with(|transaction| -> crate::Result<()> {
            let before = policy_in(
                transaction.connection(),
                fixture.db.server_id(),
                INITIAL_TIME_MS,
                Some(&subject),
            )?;
            assert_eq!(before.check(Capability::TasksView, None), Decision::Allow);
            transaction.connection().execute(
                "UPDATE devices SET revoked_at = ? WHERE id = ? AND server_id = ?",
                params![
                    INITIAL_TIME_MS,
                    subject.device.as_str(),
                    fixture.db.server_id()
                ],
            )?;
            let after = policy_in(
                transaction.connection(),
                fixture.db.server_id(),
                INITIAL_TIME_MS,
                Some(&subject),
            )?;
            assert_eq!(
                after.check(Capability::TasksView, None),
                Decision::Deny(DenyReason::NoIdentity)
            );
            Ok(())
        })
        .expect("resolve policy without opening a nested transaction");
    assert!(
        !fixture
            .identities
            .current_policy(Some(&subject))
            .unwrap()
            .has_current_identity()
    );
}

#[test]
fn policy_in_uses_the_supplied_server_and_transaction_time() {
    let fixture = Fixture::open();
    let subject = fixture.subject(RoleId::from(BuiltinRole::Viewer));
    fixture.stored_override(
        &subject,
        Capability::TasksView,
        "*",
        Effect::Deny,
        Some(INITIAL_TIME_MS + 10),
    );
    let foreign_server = uuid::Uuid::new_v4().to_string();
    fixture
        .db
        .read_with(|snapshot| -> crate::Result<()> {
            let resolve =
                |server, time, subject| policy_in(snapshot.connection(), server, time, subject);
            assert_eq!(
                resolve(fixture.db.server_id(), INITIAL_TIME_MS, Some(&subject))?
                    .check(Capability::TasksView, None),
                Decision::Deny(DenyReason::ExplicitDeny)
            );
            assert_eq!(
                resolve(fixture.db.server_id(), INITIAL_TIME_MS + 10, Some(&subject))?
                    .check(Capability::TasksView, None),
                Decision::Allow
            );
            assert!(
                !resolve(&foreign_server, INITIAL_TIME_MS, Some(&subject))?.has_current_identity()
            );
            assert!(
                !resolve(fixture.db.server_id(), INITIAL_TIME_MS, None)?.has_current_identity()
            );
            Ok(())
        })
        .expect("resolve server-bound policy at the supplied time");
}

#[test]
fn policy_in_stays_in_the_read_snapshot_then_refreshes_on_the_next_read() {
    let fixture = Fixture::open();
    let subject = fixture.subject(RoleId::from(BuiltinRole::Viewer));
    fixture
        .db
        .read_with(|snapshot| -> crate::Result<()> {
            let before = policy_in(
                snapshot.connection(),
                fixture.db.server_id(),
                INITIAL_TIME_MS,
                Some(&subject),
            )?;
            assert_eq!(before.check(Capability::TasksView, None), Decision::Allow);
            let writer = fixture.db.clone();
            let device = subject.device.clone();
            std::thread::spawn(move || {
                writer.write(|connection| {
                    assert_eq!(
                        connection.execute(
                            "UPDATE devices SET revoked_at = ? WHERE id = ? AND server_id = ?",
                            params![INITIAL_TIME_MS, device.as_str(), writer.server_id()],
                        )?,
                        1
                    );
                    Ok(())
                })
            })
            .join()
            .expect("fixture writer returned without panic")
            .expect("revocation committed while the read snapshot remained open");
            let after = policy_in(
                snapshot.connection(),
                fixture.db.server_id(),
                INITIAL_TIME_MS,
                Some(&subject),
            )?;
            assert_eq!(after.check(Capability::TasksView, None), Decision::Allow);
            Ok(())
        })
        .expect("policy and data share the admitted snapshot");
    fixture
        .db
        .read_with(|snapshot| -> crate::Result<()> {
            let refreshed = policy_in(
                snapshot.connection(),
                fixture.db.server_id(),
                INITIAL_TIME_MS,
                Some(&subject),
            )?;
            assert_eq!(
                refreshed.check(Capability::TasksView, None),
                Decision::Deny(DenyReason::NoIdentity)
            );
            Ok(())
        })
        .expect("next snapshot observes committed revocation");
}
