use super::*;
use crate::config::MetricsSettings;
use std::sync::Arc;
use vesta_state::{StateDatabase, StateDirectory, SystemClock};
fn database() -> (tempfile::TempDir, StateDatabase) {
    let root = tempfile::tempdir().unwrap();
    let db = StateDatabase::open(
        StateDirectory::new(root.path().join("state"), None).unwrap(),
        Arc::new(SystemClock),
    )
    .unwrap();
    (root, db)
}
fn sample(at: i64, value: Option<f64>) -> Sample {
    let mut values = [None; 17];
    values[0] = value;
    values[9] = Some(3.0);
    Sample::new(
        at,
        Some("01".repeat(16)),
        "running".into(),
        "ready".into(),
        values,
    )
}
fn event(at: i64, name: &str) -> Activity {
    Activity {
        id: 0,
        at,
        generation: Some("01".repeat(16)),
        name: Some(name.into()),
        kind: "join".into(),
    }
}
#[test]
fn metrics_activity_is_anchored_and_rejects_chat_impersonation() {
    assert_eq!(
        activity_line("[12:34:56] [Server thread/INFO]: Alex joined the game"),
        Some(("Alex", "join"))
    );
    assert_eq!(
        activity_line("[12:34:56] [Server thread/INFO]: Bob_1 left the game"),
        Some(("Bob_1", "leave"))
    );
    // Paper, Purpur and Folia print this console form instead.
    assert_eq!(
        activity_line("[12:34:56 INFO]: Alex joined the game"),
        Some(("Alex", "join"))
    );
    assert_eq!(
        activity_line(
            "[12:34:56] [Server thread/INFO] [minecraft/MinecraftServer]: Bob_1 left the game"
        ),
        Some(("Bob_1", "leave"))
    );
    for line in [
        "[12:34:56] [Server thread/INFO]: <Bob> Alex joined the game",
        "Alex joined the game",
        "[12:34:56] [Async thread/INFO]: Alex joined the game",
        "[12:34:56] [Server thread/WARN]: Alex joined the game",
        "[12:34:56] [Server thread/INFO]: Alex joined the game extra",
        "[12:34:56] [Server thread/INFO]: @a joined the game",
        "[12:34:56 INFO]: <Bob> Alex joined the game",
    ] {
        assert!(activity_line(line).is_none());
    }
}
#[test]
fn metrics_aggregation_restart_counts_unknowns_and_daily_uniqueness() {
    let (_root, db) = database();
    let at = 100 * DAY;
    db.write(|c| {
        storage::record(
            c,
            Some(&sample(at, Some(10.0))),
            &[event(at, "Alex")],
            at,
            MetricsSettings::default(),
        )
    })
    .unwrap();
    db.write(|c| {
        storage::record(
            c,
            Some(&sample(at, Some(10.0))),
            &[],
            at,
            MetricsSettings::default(),
        )
    })
    .unwrap();
    let dir = db.directory().clone();
    db.close().unwrap();
    let db = StateDatabase::open(dir, Arc::new(SystemClock)).unwrap();
    db.write(|c| {
        storage::record(
            c,
            Some(&sample(at + 10000, Some(30.0))),
            &[event(at + 10000, "alex")],
            at + 10000,
            MetricsSettings::default(),
        )
    })
    .unwrap();
    db.write(|c| {
        storage::record(
            c,
            Some(&sample(at + 20000, None)),
            &[event(at + 20000, "Steve")],
            at + 20000,
            MetricsSettings::default(),
        )
    })
    .unwrap();
    let range = db
        .read(|c| storage::range(c, "game_cpu_percent", at, at + DAY, Resolution::Daily))
        .unwrap();
    assert_eq!(range.points.len(), 1);
    let point = &range.points[0];
    assert_eq!(point.numeric.known_count, 2);
    assert_eq!(point.numeric.unknown_count, 1);
    assert_eq!(point.numeric.min, Some(10.0));
    assert_eq!(point.numeric.avg, Some(20.0));
    assert_eq!(point.numeric.max, Some(30.0));
    assert_eq!(point.unique_joining_players, Some(2));
    assert_eq!(point.joins, 3);
    assert_eq!(point.peak_online, Some(3.0));
    let serialized = serde_json::to_string(&range).unwrap();
    assert!(!serialized.contains("Alex"));
    assert!(!serialized.contains("Steve"));
    let unknown = db
        .read(|c| storage::range(c, "tps", at, at + DAY, Resolution::FiveMinute))
        .unwrap();
    assert_eq!(unknown.points[0].numeric.unknown_count, 3);
    assert!(unknown.points[0].numeric.avg.is_none());
    let current = db.read(storage::current).unwrap();
    assert!(current.sample.unwrap().unknown.contains(&"mspt".to_owned()));
}
#[test]
fn metrics_clock_retention_ceiling_and_activity_gaps_are_visible() {
    let (_root, db) = database();
    let at = 100 * DAY;
    let settings = MetricsSettings::default();
    db.write(|c| storage::record(c, Some(&sample(at, Some(5.0))), &[], at, settings))
        .unwrap();
    db.write(|c| {
        storage::record(
            c,
            Some(&sample(at - 1000, Some(6.0))),
            &[],
            at - 1000,
            settings,
        )
    })
    .unwrap();
    assert_eq!(db.read(storage::current).unwrap().status.clock_skips, 1);
    let corrected = at - DAY;
    db.write(|c| {
        storage::record(
            c,
            Some(&sample(corrected, Some(7.0))),
            &[],
            corrected,
            settings,
        )
    })
    .unwrap();
    let current = db.read(storage::current).unwrap();
    assert_eq!(current.sample.unwrap().sampled_at, corrected);
    assert_eq!(current.status.clock_steps, 1);
    let events = db.read(|c| storage::activity(c, 0, 101)).unwrap();
    assert_eq!(events[0].kind, "clock_step");
    let point = db
        .read(|c| {
            storage::range(
                c,
                "game_cpu_percent",
                corrected,
                corrected,
                Resolution::Daily,
            )
        })
        .unwrap();
    assert!(point.points[0].unique_joining_players.is_none());
    assert_eq!(point.points[0].gaps, 1);
    // A sharply lowered budget still preserves the separately bounded current view.
    let tiny = MetricsSettings {
        max_storage_bytes: 2048,
        ..settings
    };
    db.write(|c| storage::record(c, None, &[], at + 32 * DAY, tiny))
        .unwrap();
    let current = db.read(storage::current).unwrap();
    assert!(current.status.pruned_rows > 0);
    assert!(current.status.pruned_bytes > 0);
    assert!(current.status.storage_bytes <= 2048);
    assert!(current.sample.is_some());
    assert!(
        db.read(|c| storage::range(c, "game_cpu_percent", 0, at + 2 * DAY, Resolution::Raw))
            .unwrap()
            .points
            .is_empty()
    );
    assert!(
        db.read(|c| storage::activity(c, 0, 101))
            .unwrap()
            .is_empty()
    );
    assert!(current.status.activity_pruned);
}
#[test]
fn metrics_range_refuses_excess_points_and_closed_series_bypasses() {
    let (_root, db) = database();
    db.write(|c| {
        for at in 0..2001 {c.execute("INSERT INTO metrics_raw(sampled_at,phase,readiness,bytes) VALUES(?,'stopped','stopped',0)",[at])?;}
        Ok(())
    }).unwrap();
    assert_eq!(
        db.read(|c| storage::range(c, "tps", 0, 2001, Resolution::Raw))
            .err()
            .unwrap()
            .code(),
        "state_metrics_invalid_request"
    );
    assert!(
        db.read(|c| storage::range(c, "tps FROM metrics_raw;--", 0, 1, Resolution::Raw))
            .is_err()
    );
    assert_eq!(Resolution::Auto.select(0, DAY), Resolution::FiveMinute);
    assert_eq!(Resolution::Auto.select(0, 30 * DAY), Resolution::Daily);
}
