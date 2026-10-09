//! Bounded assertions for foundation limits that no existing target covers.
//!
//! Every case drives the published `vesta_state` API over a real temporary SQLite
//! database created by this file's own fixture. SQL tampering is confined to those
//! fixtures and to the append-only audit triggers a local administrator could drop;
//! no production constraint is relaxed and no unexecuted behavior is asserted.
//!
//! These are new assertions over already-reviewed production code. They are not a
//! final audit, not requirement-mapping evidence, and not permission to promote an
//! inventory row.
mod support;

use rusqlite::Connection;
use support::Fixture;
use vesta_state::audit::{AuditActor, AuditLog, AuditRecord, MAX_PAGE};
use vesta_state::events::{Bounds, EventLog, MAX_PAYLOAD_CHARS};
use vesta_state::settings::{ApplyMode, MAX_VALUE_CHARS, SettingsStore};

const AT_MS: i64 = 1_000;
const EARLIER_MS: i64 = 10;

fn actor() -> AuditActor {
    AuditActor::system("foundation-bounds".to_owned()).expect("bounded system actor")
}

fn plain_record(actor: &AuditActor) -> AuditRecord {
    AuditRecord::of(
        actor.clone(),
        "change".to_owned(),
        "server".to_owned(),
        "ok".to_owned(),
        None,
    )
    .expect("plain fixture record")
}

fn assert_code<T>(result: vesta_state::Result<T>, expected: &str) {
    match result {
        Ok(_) => panic!("expected state error {expected}"),
        Err(error) => assert_eq!(error.code(), expected),
    }
}

fn watermark(fixture: &Fixture) -> String {
    fixture
        .db
        .read(|connection| {
            Ok(connection.query_row(
                "SELECT value FROM meta WHERE key = 'events_pruned_through'",
                [],
                |row| row.get(0),
            )?)
        })
        .expect("read the persisted retention watermark")
}

fn plant_watermark(fixture: &Fixture, value: &str) {
    fixture
        .db
        .write(|connection| {
            connection.execute(
                "UPDATE meta SET value = ?1 WHERE key = 'events_pruned_through'",
                [value],
            )?;
            Ok(())
        })
        .expect("plant a watermark in this test's own fixture")
}

fn append_events(fixture: &Fixture, count: usize) -> EventLog {
    let events = EventLog::new(fixture.db.clone());
    for _ in 0..count {
        fixture
            .db
            .write(|connection| events.append(connection, "a.kind", None, Some("{}")))
            .expect("append a fixture event");
    }
    events
}

fn audit_chain_is_valid(fixture: &Fixture) -> bool {
    AuditLog::new(fixture.db.clone())
        .expect("audit log opens only on a valid chain")
        .verify_chain()
        .expect("verify")
        .valid
}

/// A row attributed to another server is not this chain, even though its own hash and
/// its link are intact.
#[test]
fn a_row_attributed_to_a_foreign_server_is_detected_by_chain_verification() {
    let fixture = Fixture::open();
    let audit = AuditLog::new(fixture.db.clone()).expect("chain starts valid");
    audit
        .append_record(&plain_record(&actor()))
        .expect("append one record");
    assert!(audit.verify_chain().expect("verify").valid);

    // Re-attribution violates the server_identity foreign key, so this fixture-only
    // connection relaxes it to model a local administrator editing its own database file.
    let raw = Connection::open(fixture.db.directory().database_file())
        .expect("open this target's own fixture database");
    raw.pragma_update(None, "foreign_keys", "OFF")
        .expect("relax the constraint on this owned fixture only");
    raw.execute_batch("DROP TRIGGER audit_events_no_update; DROP TRIGGER audit_events_no_delete;")
        .expect("drop this fixture's append-only guards");
    raw.execute(
        "UPDATE audit_events SET server_id = '00000000-0000-4000-8000-000000000000' WHERE seq = 1",
        [],
    )
    .expect("re-attribute the owned fixture row");
    drop(raw);

    let verification = audit.verify_chain().expect("verify the tampered chain");
    assert!(!verification.valid);
    assert_eq!(verification.checked, 0);
    assert_eq!(verification.first_broken_seq, Some(1));
    assert_eq!(
        AuditLog::new(fixture.db.clone())
            .err()
            .expect("a foreign-server chain refuses to open")
            .code(),
        "state_audit_chain_corrupt"
    );
}

/// A stored link that no longer names the previous row's hash breaks the chain at that row.
#[test]
fn a_stored_link_that_does_not_name_the_previous_row_is_detected() {
    let fixture = Fixture::open();
    let audit = AuditLog::new(fixture.db.clone()).expect("chain starts valid");
    let a = actor();
    audit
        .append_record(&plain_record(&a))
        .expect("append head one");
    audit
        .append_record(&plain_record(&a))
        .expect("append head two");
    assert!(audit.verify_chain().expect("verify").valid);

    fixture
        .db
        .write(|connection| {
            connection.execute("DROP TRIGGER audit_events_no_update", [])?;
            connection.execute(
                "UPDATE audit_events SET prev_hash = ?1 WHERE seq = 2",
                ["a".repeat(64)],
            )?;
            Ok(())
        })
        .expect("break the stored link in this fixture");

    let verification = audit.verify_chain().expect("verify the tampered chain");
    assert!(!verification.valid);
    assert_eq!(verification.checked, 1, "the intact prefix still verifies");
    assert_eq!(verification.first_broken_seq, Some(2));
    assert_eq!(
        AuditLog::new(fixture.db.clone())
            .err()
            .expect("a broken link refuses to open")
            .code(),
        "state_audit_chain_corrupt"
    );
}

/// A row whose stored hash does not equal the hash of its own fields is corrupt, even
/// though the link and the recorded fields are untouched.
#[test]
fn a_stored_hash_that_does_not_match_its_own_row_is_detected() {
    let fixture = Fixture::open();
    let audit = AuditLog::new(fixture.db.clone()).expect("chain starts valid");
    let a = actor();
    audit
        .append_record(&plain_record(&a))
        .expect("append head one");
    audit
        .append_record(&plain_record(&a))
        .expect("append head two");
    let head_before = audit.page(1, 1).expect("read head").remove(0);
    assert!(audit.verify_chain().expect("verify").valid);

    fixture
        .db
        .write(|connection| {
            connection.execute("DROP TRIGGER audit_events_no_update", [])?;
            connection.execute(
                "UPDATE audit_events SET hash = ?1 WHERE seq = 2",
                ["b".repeat(64)],
            )?;
            Ok(())
        })
        .expect("rewrite only the stored hash in this fixture");

    let head_after = audit.page(1, 1).expect("read head").remove(0);
    assert_eq!(
        head_after.metadata_json, head_before.metadata_json,
        "the recorded fields are untouched"
    );
    assert_eq!(
        head_after.prev_hash, head_before.prev_hash,
        "the link is untouched"
    );
    assert_ne!(
        head_after.hash, head_before.hash,
        "only the stored hash changed"
    );

    let verification = audit.verify_chain().expect("verify the tampered chain");
    assert!(!verification.valid);
    assert_eq!(verification.checked, 1);
    assert_eq!(verification.first_broken_seq, Some(2));
    assert_eq!(
        AuditLog::new(fixture.db.clone())
            .err()
            .expect("a rewritten hash refuses to open")
            .code(),
        "state_audit_chain_corrupt"
    );
}

/// Every bounded record field accepts exactly its documented length and refuses one more
/// UTF-16 unit, so an oversized field can never reach the persisted hash chain.
#[test]
fn audit_record_field_bounds_are_inclusive_and_refuse_one_more_unit() {
    let a = actor();
    let none: Vec<(String, Option<String>)> = Vec::new();

    let at_bounds = AuditRecord::new(
        a.clone(),
        "a".repeat(96),
        "t".repeat(512),
        "o".repeat(32),
        Some("i".repeat(64)),
        none.clone(),
    )
    .expect("exactly the documented field bounds are accepted");
    assert_eq!(at_bounds.metadata_json(), "{}");

    let one_past = [
        (
            "a".repeat(97),
            "t".repeat(512),
            "o".repeat(32),
            Some("i".repeat(64)),
        ),
        (
            "a".repeat(96),
            "t".repeat(513),
            "o".repeat(32),
            Some("i".repeat(64)),
        ),
        (
            "a".repeat(96),
            "t".repeat(512),
            "o".repeat(33),
            Some("i".repeat(64)),
        ),
        (
            "a".repeat(96),
            "t".repeat(512),
            "o".repeat(32),
            Some("i".repeat(65)),
        ),
    ];
    for (action, target, outcome, operation_id) in one_past {
        let error = AuditRecord::new(
            a.clone(),
            action,
            target,
            outcome,
            operation_id,
            none.clone(),
        )
        .err()
        .expect("one unit past a field bound must be refused");
        assert_eq!(error.code(), "state_invalid_request");
    }
}

/// Two entries for one key would make the canonical metadata depend on argument order, so
/// the repetition is refused instead of silently overwriting the first entry.
#[test]
fn a_repeated_metadata_key_is_refused_instead_of_silently_overwriting() {
    let repeated = AuditRecord::new(
        actor(),
        "change".to_owned(),
        "server".to_owned(),
        "ok".to_owned(),
        None,
        [
            ("same".to_owned(), Some("first".to_owned())),
            ("same".to_owned(), Some("second".to_owned())),
        ],
    )
    .err()
    .expect("a duplicate metadata key must be refused");
    assert_eq!(repeated.code(), "state_invalid_request");

    // Distinct keys are unaffected, so the refusal is specific to repetition.
    let distinct = AuditRecord::new(
        actor(),
        "change".to_owned(),
        "server".to_owned(),
        "ok".to_owned(),
        None,
        [
            ("b".to_owned(), Some("2".to_owned())),
            ("a".to_owned(), Some("1".to_owned())),
        ],
    )
    .expect("distinct metadata keys are accepted");
    assert_eq!(distinct.metadata_json(), r#"{"a":"1","b":"2"}"#);
}

/// A paging window outside 1..=MAX_PAGE is clamped into that window rather than refused,
/// and clamping is a read that never changes the chain it reports.
#[test]
fn audit_page_limits_are_clamped_into_the_documented_window() {
    let fixture = Fixture::open();
    let audit = AuditLog::new(fixture.db.clone()).expect("chain starts valid");
    let record = plain_record(&actor());
    fixture
        .db
        .write(|connection| {
            for _ in 0..=MAX_PAGE {
                audit.append(connection, &record)?;
            }
            Ok(())
        })
        .expect("append more than one full page in the owned fixture");
    assert_eq!(MAX_PAGE, 500);

    for limit in [0, -1, i32::MIN] {
        let page = audit.page(0, limit).unwrap_or_else(|_| {
            panic!("limit {limit} is clamped, never refused");
        });
        assert_eq!(page.len(), 1, "limit {limit} clamps up to one row");
    }
    for limit in [MAX_PAGE as i32, MAX_PAGE as i32 + 1, i32::MAX] {
        let page = audit.page(0, limit).unwrap_or_else(|_| {
            panic!("limit {limit} is clamped, never refused");
        });
        assert_eq!(
            page.len(),
            MAX_PAGE,
            "limit {limit} cannot return the extra row"
        );
        assert_eq!(page.last().unwrap().seq, MAX_PAGE as i64);
    }

    let tail = audit
        .page(MAX_PAGE as i64, MAX_PAGE as i32)
        .expect("paged tail");
    assert_eq!(tail.len(), 1);
    assert_eq!(tail[0].seq, MAX_PAGE as i64 + 1);
    assert!(
        audit
            .page(MAX_PAGE as i64 + 1, MAX_PAGE as i32)
            .expect("past the head")
            .is_empty()
    );
    assert!(audit.verify_chain().expect("clamping never mutates").valid);
}

/// The plain-text setting bound is counted in UTF-16 units and is inclusive; an over-bound
/// value is refused before any row or audit record is written.
#[test]
fn a_setting_value_at_the_bound_is_stored_and_one_more_unit_is_refused_whole() {
    let fixture = Fixture::open();
    let settings = SettingsStore::new(fixture.db.clone()).expect("bind settings to the fixture");
    let a = actor();
    assert_eq!(MAX_VALUE_CHARS, 4_096);

    let accepted = settings
        .set_desired(
            &a,
            "fixture.at_bound",
            &"v".repeat(MAX_VALUE_CHARS),
            ApplyMode::Manual,
        )
        .expect("exactly MAX_VALUE_CHARS units is accepted");
    assert_eq!(accepted.desired_value.len(), MAX_VALUE_CHARS);
    assert!(accepted.pending(), "a desired value is not yet effective");

    assert_code(
        settings.set_desired(
            &a,
            "fixture.over_bound",
            &"v".repeat(MAX_VALUE_CHARS + 1),
            ApplyMode::Manual,
        ),
        "state_invalid_request",
    );
    assert!(
        settings
            .get("fixture.over_bound")
            .expect("read the refused key")
            .is_none(),
        "a refused value stores no row"
    );

    // The bound counts UTF-16 units, not bytes: half as many supplementary scalars, and
    // each one is a 4-byte UTF-8 scalar, so the stored bytes exceed the unit bound.
    let at_units = "😀".repeat(MAX_VALUE_CHARS / 2);
    let accepted_units = settings
        .set_desired(&a, "fixture.scalars", &at_units, ApplyMode::Manual)
        .expect("MAX_VALUE_CHARS UTF-16 units of supplementary scalars is accepted");
    assert_eq!(accepted_units.desired_value.len(), MAX_VALUE_CHARS * 2);
    assert_code(
        settings.set_desired(
            &a,
            "fixture.scalars_over",
            &format!("{at_units}😀"),
            ApplyMode::Manual,
        ),
        "state_invalid_request",
    );

    // The refusals left the accepted rows untouched and recorded no orphan audit entries.
    let persisted = settings
        .get("fixture.at_bound")
        .expect("read the accepted key")
        .expect("the accepted row survives");
    assert_eq!(persisted.desired_value.len(), MAX_VALUE_CHARS);
    let audit = AuditLog::new(fixture.db.clone()).expect("chain still valid");
    assert_eq!(audit.page(0, MAX_PAGE as i32).expect("read audit").len(), 2);
}

/// The persisted retention watermark only ever moves forward: a repeat prune of an
/// already-pruned prefix deletes nothing and leaves the watermark exactly where it was.
#[test]
fn the_retention_watermark_advances_once_and_holds_across_a_repeat_prune() {
    let fixture = Fixture::with_time(AT_MS);
    let events = append_events(&fixture, 5);
    assert_eq!(watermark(&fixture), "0");

    assert_eq!(events.prune_older_than(AT_MS + 1).expect("first prune"), 5);
    assert_eq!(watermark(&fixture), "5");
    assert!(
        audit_chain_is_valid(&fixture),
        "pruning leaves the audit chain intact"
    );

    // These rows are newer than this cutoff, so the computed prefix is exactly the one
    // already removed: nothing is deleted and the watermark must not move.
    append_events(&fixture, 3);
    assert_eq!(
        events.prune_older_than(EARLIER_MS).expect("repeat prune"),
        0
    );
    assert_eq!(
        watermark(&fixture),
        "5",
        "a repeat prune never lowers the watermark"
    );

    // A genuinely later prefix still advances it.
    assert_eq!(events.prune_older_than(AT_MS + 1).expect("later prune"), 3);
    assert_eq!(watermark(&fixture), "8");
}

/// The monotonic guard is load-bearing: without it a lower computed prefix would rewrite
/// an already-higher persisted watermark and re-expose pruned cursors to clients.
#[test]
fn a_computed_prune_never_rewrites_a_higher_persisted_watermark() {
    let fixture = Fixture::with_time(AT_MS);
    let events = append_events(&fixture, 3);

    // A watermark ahead of every live row: unreachable by pruning, planted to prove the guard.
    plant_watermark(&fixture, "9");
    assert_eq!(
        events.prune_older_than(AT_MS + 1).expect("guarded prune"),
        3
    );
    assert_eq!(watermark(&fixture), "9", "the watermark never regresses");

    // With nothing left to prune the computed prefix is zero, which writes nothing at all.
    plant_watermark(&fixture, "4");
    assert_eq!(events.prune_older_than(AT_MS + 1).expect("empty prune"), 0);
    assert_eq!(watermark(&fixture), "4");

    // From a fresh journal a stored value below the computed prefix still advances forward.
    let second = Fixture::with_time(AT_MS);
    let second_events = append_events(&second, 3);
    plant_watermark(&second, "1");
    assert_eq!(
        second_events.prune_older_than(AT_MS + 1).expect("advance"),
        3
    );
    assert_eq!(
        watermark(&second),
        "3",
        "a stored value below the prefix advances"
    );
    assert!(audit_chain_is_valid(&second));
}

/// A prune that stops inside the stream reports the new lowest retained cursor, so a
/// resuming client learns both the gap and where it may continue.
#[test]
fn a_partial_prune_reports_the_new_oldest_retained_cursor() {
    let fixture = Fixture::with_time(AT_MS);
    let events = append_events(&fixture, 5);
    fixture
        .db
        .write(|connection| {
            // Expire only the oldest two rows, so the prune stops inside the stream.
            connection.execute(
                "UPDATE events SET at = ?1 WHERE cursor IN (1, 2)",
                [EARLIER_MS],
            )?;
            Ok(())
        })
        .expect("expire only the oldest two rows in this fixture");

    assert_eq!(
        fixture
            .db
            .read(|connection| events.bounds(connection))
            .expect("bounds before pruning"),
        Bounds {
            pruned_through: 0,
            oldest_retained: 1,
            latest: 5,
        }
    );

    assert_eq!(
        events.prune_older_than(AT_MS - 1).expect("partial prune"),
        2
    );
    assert_eq!(watermark(&fixture), "2");
    assert_eq!(
        fixture
            .db
            .read(|connection| events.bounds(connection))
            .expect("bounds after a partial prune"),
        Bounds {
            pruned_through: 2,
            oldest_retained: 3,
            latest: 5,
        },
        "only the expired prefix is gone; the rest of the stream is retained"
    );

    let page = events.read(0, 10).expect("read after a partial prune");
    assert!(page.gap, "the removed prefix is reported as a gap");
    assert_eq!(
        page.events
            .iter()
            .map(|event| event.cursor)
            .collect::<Vec<_>>(),
        vec![3, 4, 5]
    );
    assert_eq!(page.next_cursor, 5);
    assert_eq!(page.oldest_available_cursor, 3);

    let rows = fixture
        .db
        .read(|connection| events.scan(connection, 0, 100, 10, &["a.kind"], MAX_PAYLOAD_CHARS))
        .expect("bounded scan after a partial prune");
    assert_eq!(
        rows.iter().map(|row| row.cursor).collect::<Vec<_>>(),
        vec![3, 4, 5],
        "a bounded scan agrees with the reported boundaries"
    );
    assert!(audit_chain_is_valid(&fixture));
}
