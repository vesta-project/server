//! Private unit fixtures over actual protected SQLite, audit/event logs and
//! canonical identity resolution. Trusted SQL seeds facts; there is no injectable
//! production authority, public test feature or executable fixture handler.
use super::*;
use crate::identity::{DeviceId, PrincipalId};
use rusqlite::{Connection, params};
use sha2::{Digest, Sha256};
use std::sync::{
    Arc,
    atomic::{AtomicI64, Ordering},
};
use tempfile::TempDir;
use vesta_state::{Clock, StateDirectory};

mod faults;
mod launch_configuration;
mod ledger;
mod review_regressions;
mod scheduling;

const INITIAL_MS: i64 = 1_791_115_200_000;
struct TestClock(AtomicI64);
impl TestClock {
    fn new() -> Self {
        Self(AtomicI64::new(INITIAL_MS))
    }
    fn set(&self, value: i64) {
        self.0.store(value, Ordering::SeqCst);
    }
}
impl Clock for TestClock {
    fn millis(&self) -> i64 {
        self.0.load(Ordering::SeqCst)
    }
}

struct Fixture {
    _root: TempDir,
    clock: Arc<TestClock>,
    db: StateDatabase,
    ops: Operations,
    subject: Subject,
}
impl Fixture {
    fn open() -> Self {
        let root = tempfile::tempdir().expect("create local operations test root");
        let clock = Arc::new(TestClock::new());
        let directory = StateDirectory::new(root.path().join("private state"), None)
            .expect("protect test directory");
        let db = StateDatabase::open(directory, clock.clone()).expect("open actual state database");
        let subject = seed_subject(&db, true);
        let ops = Operations::new(db.clone()).expect("verify actual audit log");
        Self {
            _root: root,
            clock,
            db,
            ops,
            subject,
        }
    }
    fn admit(&self, paths: &[&str], key: &str) -> OperationSummary {
        match self
            .ops
            .admit(
                &self.subject,
                "fixture.write",
                &raw(paths, "requested"),
                Some(key),
            )
            .expect("admit fixture action")
        {
            Admission::Created(summary) => summary,
            Admission::Existing(_) => panic!("fixture key must be fresh"),
        }
    }
    fn read(&self, id: &OperationId) -> OperationSummary {
        self.ops
            .get(&self.subject, id)
            .expect("read sanitized task")
            .expect("task exists")
    }
    fn sql(&self, work: impl FnOnce(&Connection) -> vesta_state::Result<()>) {
        self.db.write(work).expect("trusted fixture SQL");
    }
    fn deny(&self, subject: &Subject, capability: Capability, scope: &str) {
        self.override_grant(subject, capability, scope, "deny");
    }
    fn allow(&self, subject: &Subject, capability: Capability, scope: &str) {
        self.override_grant(subject, capability, scope, "allow");
    }
    fn override_grant(&self, subject: &Subject, capability: Capability, scope: &str, effect: &str) {
        self.sql(|c| {
            c.execute(
                "INSERT INTO principal_overrides (id,principal_id,capability,scope,effect,granted_by,created_at) VALUES (?,?,?,?,?,'fixture',?)",
                params![uuid::Uuid::new_v4().to_string(), subject.principal.as_str(), capability.id(), scope, effect, self.clock.millis()],
            )?;
            Ok(())
        });
    }
    fn counts(&self) -> (i64, i64, i64, i64) {
        self.db
            .read(|c| {
                Ok((
                    count(c, "operations")?,
                    count(c, "resource_locks")?,
                    count(c, "audit_events")?,
                    count(c, "events")?,
                ))
            })
            .expect("count actual journal rows")
    }
    fn held(&self, id: &OperationId) -> Vec<String> {
        self.db
            .read(|c| {
                let mut statement = c.prepare(
                    "SELECT resource FROM resource_locks WHERE operation_id=? ORDER BY resource",
                )?;
                let rows = statement.query_map([id.as_str()], |row| row.get(0))?;
                Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
            })
            .expect("inspect trusted fixture locks")
    }
    fn owner(&self, id: &OperationId) -> Option<String> {
        self.db
            .read(|c| {
                Ok(c.query_row(
                    "SELECT owner_token FROM operations WHERE id=?",
                    [id.as_str()],
                    |row| row.get(0),
                )?)
            })
            .expect("inspect private fixture owner")
    }
    fn error_code(&self, id: &OperationId) -> Option<String> {
        self.db
            .read(|c| {
                Ok(c.query_row(
                    "SELECT error_code FROM operations WHERE id=?",
                    [id.as_str()],
                    |row| row.get(0),
                )?)
            })
            .expect("inspect fixed operation failure code")
    }
    fn input_status(&self, id: &OperationId) -> (Option<String>, String) {
        self.db
            .read(|c| {
                Ok(c.query_row(
                    "SELECT input_json,input_status FROM operations WHERE id=?",
                    [id.as_str()],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )?)
            })
            .expect("inspect trusted retained input")
    }
    fn terminal(&self, id: &OperationId, effect_started: bool) {
        self.sql(|c| {
            c.execute("DELETE FROM resource_locks WHERE operation_id=?", [id.as_str()])?;
            c.execute(
                "UPDATE operations SET state='succeeded',owner_token=NULL,finished_at=?,updated_at=?,external_effect_started=? WHERE id=?",
                params![self.clock.millis(), self.clock.millis(), effect_started, id.as_str()],
            )?;
            Ok(())
        });
    }
    fn seed_legacy(
        &self,
        id: &str,
        state: &str,
        started: bool,
        idempotent: bool,
        resource: Option<&str>,
    ) -> OperationId {
        self.sql(|c| {
            c.execute(
                "INSERT INTO operations (id,server_id,actor_principal_id,device_id,source,type,target,payload_hash,state,\
                 requires_confirmation,external_effect_idempotent,external_effect_started,cancel_requested,created_at,updated_at) \
                 VALUES (?,?,?,?,?,'legacy.operation','legacy-target','legacy-payload-hash',?,0,?,?,?, ?,?)",
                params![id, self.db.server_id(), self.subject.principal.as_str(), self.subject.device.as_str(), REQUEST_SOURCE,
                    state, idempotent, started, state == "cancelling", self.clock.millis(), self.clock.millis()],
            )?;
            if let Some(resource) = resource {
                c.execute("INSERT INTO operation_resources (operation_id,resource) VALUES (?,?)", params![id, resource])?;
                if matches!(state, "validating" | "running" | "cancelling" | "recovery_required") {
                    c.execute("INSERT INTO resource_locks (resource,operation_id,acquired_at) VALUES (?,?,?)", params![resource,id,self.clock.millis()])?;
                }
            }
            Ok(())
        });
        OperationId::parse(id).expect("legacy fixture id")
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let closed = self.db.close();
        if !std::thread::panicking() {
            closed.expect("drain and close actual test database");
        }
    }
}
fn count(c: &Connection, table: &'static str) -> vesta_state::Result<i64> {
    Ok(
        c.query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
            row.get(0)
        })?,
    )
}
fn raw(paths: &[&str], value: &str) -> String {
    serde_json::json!({ "version": 1, "paths": paths, "value": value }).to_string()
}
fn seed_subject(db: &StateDatabase, owner: bool) -> Subject {
    let principal =
        PrincipalId::parse(&uuid::Uuid::new_v4().to_string()).expect("fixture principal");
    let device = DeviceId::parse(&uuid::Uuid::new_v4().to_string()).expect("fixture device");
    db.write(|c| {
        c.execute("INSERT INTO principals (id,server_id,display_name,status,created_at,updated_at) VALUES (?,?,'Fixture','active',?,?)",
            params![principal.as_str(),db.server_id(),db.clock().millis(),db.clock().millis()])?;
        c.execute("INSERT INTO devices (id,server_id,principal_id,label,public_key_sha256,created_at) VALUES (?,?,?,'Fixture',?,?)",
            params![device.as_str(),db.server_id(),principal.as_str(),hex::encode(Sha256::digest(device.as_str().as_bytes())),db.clock().millis()])?;
        if owner {
            c.execute("INSERT INTO role_assignments (id,principal_id,role_id,granted_by,created_at) VALUES (?,?,'owner','fixture',?)",
                params![uuid::Uuid::new_v4().to_string(),principal.as_str(),db.clock().millis()])?;
        }
        Ok(())
    }).expect("trusted unit-only identity facts");
    Subject { principal, device }
}
fn another_device(f: &Fixture) -> Subject {
    let device = DeviceId::parse(&uuid::Uuid::new_v4().to_string()).expect("second fixture device");
    f.sql(|c| {
        c.execute("INSERT INTO devices (id,server_id,principal_id,label,public_key_sha256,created_at) VALUES (?,?,?,'Other device',?,?)",
            params![device.as_str(),f.db.server_id(),f.subject.principal.as_str(),hex::encode(Sha256::digest(device.as_str().as_bytes())),f.clock.millis()])?;
        Ok(())
    });
    Subject {
        principal: f.subject.principal.clone(),
        device,
    }
}
fn code<T: std::fmt::Debug>(result: OperationsResult<T>, expected: &str) -> OperationError {
    let error = result.expect_err("operation must refuse");
    assert_eq!(error.code(), expected);
    error
}
