//! Bounded expiry-sweep behavior against public identity APIs and real SQLite rows.
use rusqlite::params;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{
    Arc,
    atomic::{AtomicI64, Ordering},
};
use tempfile::TempDir;
use vesta_control::identity::{
    BuiltinRole, Capability, Decision, DenyReason, DeviceId, IdentityService, PrincipalId, Subject,
};
use vesta_state::audit::AuditLog;
use vesta_state::{Clock, StateDatabase, StateDirectory};

const NOW_MS: i64 = 10_000;
const ELIGIBLE_PER_TABLE: usize = 507;
const FIRST_BATCH_PER_TABLE: usize = 500;
const FIXTURE_ACTOR: &str = "10000000-0000-4000-8000-000000000001";
const FIXTURE_DEVICE: &str = "10000000-0000-4000-8000-000000000002";
const FIXTURE_TARGET: &str = "10000000-0000-4000-8000-000000000003";
const ASSIGNMENT_PREFIX: &str = "20000000";
const OVERRIDE_PREFIX: &str = "30000000";

struct FixedClock(AtomicI64);

impl FixedClock {
    fn new(now_ms: i64) -> Self {
        Self(AtomicI64::new(now_ms))
    }
}

impl Clock for FixedClock {
    fn millis(&self) -> i64 {
        self.0.load(Ordering::SeqCst)
    }
}

struct Fixture {
    // Keep the local temporary filesystem alive until all DB handles are dropped.
    _root: TempDir,
    db: StateDatabase,
    identities: IdentityService,
    audit: AuditLog,
    actor: Subject,
}

impl Fixture {
    fn open() -> Self {
        let root = tempfile::tempdir().expect("create local expiry fixture directory");
        let state_path = root.path().join("private state");
        let game_path = root.path().join("game root");
        let directory = StateDirectory::new(&state_path, Some(&game_path))
            .expect("create separate state and game paths");
        let db = StateDatabase::open(directory, Arc::new(FixedClock::new(NOW_MS)))
            .expect("open real SQLite state");
        let identities = IdentityService::new(db.clone()).expect("create public identity service");
        let audit = AuditLog::new(db.clone()).expect("open real audit chain");
        let actor = Subject {
            principal: PrincipalId::parse(FIXTURE_ACTOR).expect("fixture actor ID"),
            device: DeviceId::parse(FIXTURE_DEVICE).expect("fixture device ID"),
        };
        let target = PrincipalId::parse(FIXTURE_TARGET).expect("fixture target ID");
        let server_id = db.server_id().to_owned();

        // Trusted fixture SQL establishes lookup records and history controls only; the
        // public service still resolves the expired actor's current policy from SQLite.
        db.write(|connection| {
            connection.execute(
                "INSERT INTO principals (id, server_id, display_name, status, created_at, updated_at) \
                 VALUES (?1, ?2, 'Expired administrator', 'active', 1, 1)",
                params![actor.principal.as_str(), server_id],
            )?;
            connection.execute(
                "INSERT INTO devices (id, server_id, principal_id, label, public_key_sha256, created_at) \
                 VALUES (?1, ?2, ?3, 'Fixture device', ?4, 1)",
                params![
                    actor.device.as_str(),
                    server_id,
                    actor.principal.as_str(),
                    "a".repeat(64)
                ],
            )?;
            // This role is expired already and its evidence was recorded earlier. The
            // sweep must not recount it, but policy must deny it even before any sweep.
            connection.execute(
                "INSERT INTO role_assignments \
                 (id, principal_id, role_id, granted_by, created_at, expires_at, expiry_recorded_at) \
                 VALUES ('10000000-0000-4000-8000-000000000005', ?1, 'administrator', \
                         'trusted-fixture', 1, 9_000, 9_500)",
                params![actor.principal.as_str()],
            )?;
            connection.execute(
                "INSERT INTO principals (id, server_id, display_name, status, created_at, updated_at) \
                 VALUES (?1, ?2, 'Active sweep target', 'active', 1, 1)",
                params![target.as_str(), server_id],
            )?;

            for index in 0..ELIGIBLE_PER_TABLE {
                let expires_at = eligible_expiry(index);
                connection.execute(
                    "INSERT INTO role_assignments \
                     (id, principal_id, role_id, granted_by, created_at, expires_at) \
                     VALUES (?1, ?2, 'viewer', 'trusted-fixture', 1, ?3)",
                    params![assignment_id(index), target.as_str(), expires_at],
                )?;
                connection.execute(
                    "INSERT INTO principal_overrides \
                     (id, principal_id, capability, scope, effect, granted_by, created_at, expires_at) \
                     VALUES (?1, ?2, 'players.view', '*', 'allow', 'trusted-fixture', 1, ?3)",
                    params![override_id(index), target.as_str(), expires_at],
                )?;
            }

            // Revoked, already-recorded, and future-expiring rows are controls in both
            // tables. None may be marked or produce expiry evidence in the sweep.
            connection.execute(
                "INSERT INTO role_assignments \
                 (id, principal_id, role_id, granted_by, created_at, expires_at, revoked_at) \
                 VALUES ('90000000-0000-4000-8000-000000000001', ?1, 'viewer', 'trusted-fixture', 1, 1, 9_000)",
                params![target.as_str()],
            )?;
            connection.execute(
                "INSERT INTO role_assignments \
                 (id, principal_id, role_id, granted_by, created_at, expires_at, expiry_recorded_at) \
                 VALUES ('90000000-0000-4000-8000-000000000002', ?1, 'viewer', 'trusted-fixture', 1, 1, 9_000)",
                params![target.as_str()],
            )?;
            connection.execute(
                "INSERT INTO role_assignments \
                 (id, principal_id, role_id, granted_by, created_at, expires_at) \
                 VALUES ('90000000-0000-4000-8000-000000000003', ?1, 'viewer', 'trusted-fixture', 1, 10_001)",
                params![target.as_str()],
            )?;
            connection.execute(
                "INSERT INTO principal_overrides \
                 (id, principal_id, capability, scope, effect, granted_by, created_at, expires_at, revoked_at) \
                 VALUES ('90000000-0000-4000-8000-000000000011', ?1, 'players.view', '*', 'allow', 'trusted-fixture', 1, 1, 9_000)",
                params![target.as_str()],
            )?;
            connection.execute(
                "INSERT INTO principal_overrides \
                 (id, principal_id, capability, scope, effect, granted_by, created_at, expires_at, expiry_recorded_at) \
                 VALUES ('90000000-0000-4000-8000-000000000012', ?1, 'players.view', '*', 'allow', 'trusted-fixture', 1, 1, 9_000)",
                params![target.as_str()],
            )?;
            connection.execute(
                "INSERT INTO principal_overrides \
                 (id, principal_id, capability, scope, effect, granted_by, created_at, expires_at) \
                 VALUES ('90000000-0000-4000-8000-000000000013', ?1, 'players.view', '*', 'allow', 'trusted-fixture', 1, 10_001)",
                params![target.as_str()],
            )?;
            Ok(())
        })
        .expect("seed valid expired grants and history controls");

        Self {
            _root: root,
            db,
            identities,
            audit,
            actor,
        }
    }

    fn markers(&self, table: &str, prefix: &str) -> BTreeMap<String, Option<i64>> {
        assert!(matches!(table, "role_assignments" | "principal_overrides"));
        self.db
            .read(|connection| {
                let mut statement = connection.prepare(&format!(
                    "SELECT id, expiry_recorded_at FROM {table} WHERE id LIKE ?1 || '-%'"
                ))?;
                let rows = statement.query_map([prefix], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, Option<i64>>(1)?))
                })?;
                let markers = rows.collect::<rusqlite::Result<BTreeMap<_, _>>>()?;
                Ok(markers)
            })
            .expect("read expiry markers")
    }

    fn marker(&self, table: &str, id: &str) -> Option<i64> {
        assert!(matches!(table, "role_assignments" | "principal_overrides"));
        self.db
            .read(|connection| {
                connection
                    .query_row(
                        &format!("SELECT expiry_recorded_at FROM {table} WHERE id = ?1"),
                        [id],
                        |row| row.get(0),
                    )
                    .map_err(Into::into)
            })
            .expect("read history-control marker")
    }

    fn expiry_audit(&self) -> Vec<(String, String, String, String, String)> {
        self.db
            .read(|connection| {
                let mut statement = connection.prepare(
                    "SELECT principal_id, source, target, outcome, metadata_json \
                     FROM audit_events WHERE action = 'authority.expired' ORDER BY seq",
                )?;
                let rows = statement.query_map([], |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                })?;
                Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
            })
            .expect("read expiry audit evidence")
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = self.db.close();
    }
}

fn eligible_expiry(index: usize) -> i64 {
    // The final seven IDs expire earlier than the first 500 IDs. This pins expiry-first
    // ordering; equal-deadline rows then exercise the deterministic lexical UUID ordering.
    if index >= FIRST_BATCH_PER_TABLE {
        1_000
    } else {
        2_000
    }
}

fn assignment_id(index: usize) -> String {
    format!("{ASSIGNMENT_PREFIX}-0000-4000-8000-{index:012x}")
}

fn override_id(index: usize) -> String {
    format!("{OVERRIDE_PREFIX}-0000-4000-8000-{index:012x}")
}

fn ordered_rows(prefix: &str) -> Vec<(i64, String)> {
    let mut rows = (0..ELIGIBLE_PER_TABLE)
        .map(|index| {
            let id = match prefix {
                ASSIGNMENT_PREFIX => assignment_id(index),
                OVERRIDE_PREFIX => override_id(index),
                _ => panic!("unknown fixture ID prefix"),
            };
            (eligible_expiry(index), id)
        })
        .collect::<Vec<_>>();
    rows.sort();
    rows
}

fn expected_audit_order() -> Vec<(String, String)> {
    let assignments = ordered_rows(ASSIGNMENT_PREFIX);
    let overrides = ordered_rows(OVERRIDE_PREFIX);
    let mut expected = Vec::with_capacity(ELIGIBLE_PER_TABLE * 2);
    for (kind, rows) in [
        ("role_assignments", &assignments),
        ("principal_overrides", &overrides),
    ] {
        expected.extend(
            rows[..FIRST_BATCH_PER_TABLE]
                .iter()
                .map(|(_, id)| (kind.to_owned(), id.clone())),
        );
    }
    for (kind, rows) in [
        ("role_assignments", &assignments),
        ("principal_overrides", &overrides),
    ] {
        expected.extend(
            rows[FIRST_BATCH_PER_TABLE..]
                .iter()
                .map(|(_, id)| (kind.to_owned(), id.clone())),
        );
    }
    expected
}

#[test]
fn expiry_sweeps_are_bounded_per_table_ordered_and_audited_once() {
    let fixture = Fixture::open();

    // The grant is already ineffective by its stored deadline; evaluation must not rely
    // on sweep markers or sweep execution to deny it.
    let policy = fixture
        .identities
        .current_policy(Some(&fixture.actor))
        .expect("resolve expired administrator policy before any sweep");
    assert!(policy.has_current_identity());
    assert_eq!(
        policy.check(Capability::CredentialsManage, None),
        Decision::Deny(DenyReason::Reserved)
    );
    assert_eq!(
        fixture.marker("role_assignments", "10000000-0000-4000-8000-000000000005"),
        Some(9_500),
        "the actor's expired grant already has prior evidence"
    );

    let assignment_rows = ordered_rows(ASSIGNMENT_PREFIX);
    let override_rows = ordered_rows(OVERRIDE_PREFIX);
    assert_eq!(
        fixture.identities.sweep_expired().expect("first sweep"),
        1_000
    );

    let assignment_markers = fixture.markers("role_assignments", ASSIGNMENT_PREFIX);
    let override_markers = fixture.markers("principal_overrides", OVERRIDE_PREFIX);
    assert_eq!(assignment_markers.len(), ELIGIBLE_PER_TABLE);
    assert_eq!(override_markers.len(), ELIGIBLE_PER_TABLE);
    let expected_first_assignments = assignment_rows[..FIRST_BATCH_PER_TABLE]
        .iter()
        .map(|(_, id)| id.as_str())
        .collect::<BTreeSet<_>>();
    let expected_first_overrides = override_rows[..FIRST_BATCH_PER_TABLE]
        .iter()
        .map(|(_, id)| id.as_str())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        assignment_markers
            .iter()
            .filter(|(_, marker)| **marker == Some(NOW_MS))
            .count(),
        FIRST_BATCH_PER_TABLE
    );
    assert_eq!(
        override_markers
            .iter()
            .filter(|(_, marker)| **marker == Some(NOW_MS))
            .count(),
        FIRST_BATCH_PER_TABLE
    );
    for (id, marker) in &assignment_markers {
        assert_eq!(
            *marker == Some(NOW_MS),
            expected_first_assignments.contains(id.as_str()),
            "unexpected first-batch assignment marker for {id}"
        );
    }
    for (id, marker) in &override_markers {
        assert_eq!(
            *marker == Some(NOW_MS),
            expected_first_overrides.contains(id.as_str()),
            "unexpected first-batch override marker for {id}"
        );
    }

    assert_eq!(
        fixture.identities.sweep_expired().expect("second sweep"),
        14
    );
    assert_eq!(fixture.identities.sweep_expired().expect("third sweep"), 0);
    assert!(
        fixture
            .markers("role_assignments", ASSIGNMENT_PREFIX)
            .values()
            .all(|marker| *marker == Some(NOW_MS))
    );
    assert!(
        fixture
            .markers("principal_overrides", OVERRIDE_PREFIX)
            .values()
            .all(|marker| *marker == Some(NOW_MS))
    );

    for (table, id) in [
        ("role_assignments", "90000000-0000-4000-8000-000000000001"),
        (
            "principal_overrides",
            "90000000-0000-4000-8000-000000000011",
        ),
    ] {
        assert_eq!(
            fixture.marker(table, id),
            None,
            "revoked history is not swept"
        );
    }
    for (table, id) in [
        ("role_assignments", "90000000-0000-4000-8000-000000000002"),
        (
            "principal_overrides",
            "90000000-0000-4000-8000-000000000012",
        ),
    ] {
        assert_eq!(
            fixture.marker(table, id),
            Some(9_000),
            "recorded history is unchanged"
        );
    }
    for (table, id) in [
        ("role_assignments", "90000000-0000-4000-8000-000000000003"),
        (
            "principal_overrides",
            "90000000-0000-4000-8000-000000000013",
        ),
    ] {
        assert_eq!(
            fixture.marker(table, id),
            None,
            "nonexpired history is not swept"
        );
    }

    let audit = fixture.audit.verify_chain().expect("verify audit chain");
    assert!(audit.valid, "expiry audit chain must be valid");
    let events = fixture.expiry_audit();
    assert_eq!(events.len(), 1_014, "each eligible grant gets one event");
    let observed = events
        .iter()
        .map(|(principal, source, target, outcome, metadata)| {
            assert_eq!(principal, "system");
            assert_eq!(source, "authority-expiry");
            assert_eq!(target, &format!("principal:{}", FIXTURE_TARGET));
            assert_eq!(outcome, "success");
            let metadata: serde_json::Value =
                serde_json::from_str(metadata).expect("valid expiry audit metadata");
            (
                metadata["kind"]
                    .as_str()
                    .expect("expiry audit kind")
                    .to_owned(),
                metadata["grant"]
                    .as_str()
                    .expect("expiry audit grant ID")
                    .to_owned(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        observed,
        expected_audit_order(),
        "sweep order is expiry then ID, per table and batch"
    );
    assert_eq!(
        observed.iter().cloned().collect::<BTreeSet<_>>().len(),
        1_014,
        "each assignment/override is audited exactly once"
    );

    // Repeated sweep is idempotent: it adds neither markers nor duplicate evidence.
    assert_eq!(
        fixture
            .identities
            .sweep_expired()
            .expect("idempotent sweep"),
        0
    );
    assert_eq!(fixture.expiry_audit().len(), 1_014);
    assert_eq!(BuiltinRole::Administrator.id(), "administrator");
}
