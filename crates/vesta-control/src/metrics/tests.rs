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
#[test]
fn metrics_default_retention_preserves_boundary_and_prunes_each_tier_after_expiry() {
    let settings = MetricsSettings::default();
    assert_eq!(settings.sample_seconds, 10);
    assert_eq!(settings.raw_retention_hours, 24);
    assert_eq!(settings.five_minute_retention_days, 30);
    assert_eq!(settings.daily_retention_days, 365);
    for (table, horizon) in [
        ("metrics_raw", DAY),
        ("metrics_5m", 30 * DAY),
        ("metrics_daily", 365 * DAY),
    ] {
        let (_root, db) = database();
        let at = 500 * DAY;
        db.write(|c| storage::record(c, Some(&sample(at, Some(5.0))), &[], at, settings))
            .unwrap();
        let count = || {
            db.read(|c| {
                Ok(
                    c.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| {
                        r.get::<_, i64>(0)
                    })?,
                )
            })
            .unwrap()
        };
        db.write(|c| storage::record(c, None, &[], at + horizon, settings))
            .unwrap();
        assert_eq!(count(), 1, "{table} retains its exact cutoff");
        db.write(|c| storage::record(c, None, &[], at + horizon + 1, settings))
            .unwrap();
        assert_eq!(count(), 0, "{table} prunes expired data");
        assert!(db.read(storage::current).unwrap().sample.is_some());
    }
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
    let skipped = db.read(storage::current).unwrap();
    assert_eq!(skipped.status.clock_skips, 1);
    assert_eq!(
        skipped.status.revision, 2,
        "status-only changes invalidate conditional reads"
    );
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

fn fetch_request(from: i64, to: i64, resolution: Resolution, max_points: usize) -> Fetch {
    Fetch {
        series: vec!["game_cpu_percent".into(), "tps".into(), "mspt".into()],
        from,
        to,
        resolution,
        max_points,
        after: None,
    }
}
#[test]
fn metrics_columnar_auto_downsampling_preserves_extrema_unknowns_and_gaps() {
    let (_root, db) = database();
    let at = 100 * DAY;
    let settings = MetricsSettings::default();
    db.write(|c| {
        for n in 0..30 {
            let value = if n == 2 { None } else { Some(n as f64) };
            let events = if n == 4 {
                vec![Activity {
                    id: 0,
                    at: at + n * 10_000,
                    generation: None,
                    name: None,
                    kind: "gap".into(),
                }]
            } else {
                Vec::new()
            };
            storage::record(
                c,
                Some(&sample(at + n * 10_000, value)),
                &events,
                at + n * 10_000,
                settings,
            )?;
        }
        Ok(())
    })
    .unwrap();
    let raw = db
        .read(|c| {
            fetch(
                c,
                &fetch_request(at, at + 290_000, Resolution::Auto, 30),
                settings,
            )
        })
        .unwrap();
    assert_eq!(raw.resolution, Resolution::Raw);
    assert_eq!(raw.columns.timestamps.len(), 30);
    let compact = db
        .read(|c| {
            fetch(
                c,
                &fetch_request(at, at + 290_000, Resolution::Raw, 5),
                settings,
            )
        })
        .unwrap();
    assert_eq!(compact.columns.timestamps.len(), 5);
    assert!(compact.columns.gaps[0]);
    let cpu = &compact.columns.series["game_cpu_percent"];
    assert_eq!(cpu.min[0], Some(0.0));
    assert_eq!(cpu.max[0], Some(5.0));
    assert_eq!(cpu.avg[0], Some(13.0 / 5.0));
    assert_eq!(cpu.known_count[0], 5);
    assert_eq!(cpu.unknown_count[0], 1);
    assert!(
        compact.columns.series["tps"]
            .avg
            .iter()
            .all(Option::is_none)
    );
    assert!(compact.columns.tick_source.iter().all(Option::is_none));
    let daily = db
        .read(|c| {
            fetch(
                c,
                &fetch_request(at, at + 10 * DAY, Resolution::Auto, 100),
                settings,
            )
        })
        .unwrap();
    assert_eq!(daily.resolution, Resolution::Daily);
    let json = serde_json::to_value(compact).unwrap();
    assert!(json["timestamps"].is_array());
    assert!(json["series"]["mspt"]["avg"][0].is_null());
    assert!(json.get("points").is_none());
}
#[test]
fn metrics_columnar_incremental_replaces_mutable_buckets_and_weights_averages() {
    let (_root, db) = database();
    let at = 100 * DAY;
    let settings = MetricsSettings::default();
    db.write(|c| storage::record(c, Some(&sample(at, Some(10.0))), &[], at, settings))
        .unwrap();
    let request = fetch_request(at, at + 300_000, Resolution::FiveMinute, 2);
    let first = db.read(|c| fetch(c, &request, settings)).unwrap();
    assert_eq!(
        first.columns.series["game_cpu_percent"].avg,
        vec![Some(10.0)]
    );
    db.write(|c| {
        storage::record(
            c,
            Some(&sample(at + 10_000, Some(30.0))),
            &[],
            at + 10_000,
            settings,
        )
    })
    .unwrap();
    let incremental = Fetch {
        after: Some(first.cursor),
        ..request.clone()
    };
    let changed = db.read(|c| fetch(c, &incremental, settings)).unwrap();
    assert_eq!(changed.columns.timestamps, vec![at]);
    assert_eq!(
        changed.columns.series["game_cpu_percent"].avg,
        vec![Some(20.0)]
    );
    let unchanged = db
        .read(|c| {
            fetch(
                c,
                &Fetch {
                    after: Some(changed.cursor),
                    ..request.clone()
                },
                settings,
            )
        })
        .unwrap();
    assert!(unchanged.columns.timestamps.is_empty());
    assert_eq!(unchanged.cursor, changed.cursor);
    db.write(|c| {
        storage::record(
            c,
            Some(&sample(at + 300_000, Some(60.0))),
            &[],
            at + 300_000,
            settings,
        )
    })
    .unwrap();
    let down = db
        .read(|c| {
            fetch(
                c,
                &fetch_request(at, at + 300_000, Resolution::FiveMinute, 1),
                settings,
            )
        })
        .unwrap();
    let cpu = &down.columns.series["game_cpu_percent"];
    assert_eq!(cpu.avg, vec![Some(100.0 / 3.0)]);
    assert_eq!(cpu.min, vec![Some(10.0)]);
    assert_eq!(cpu.max, vec![Some(60.0)]);
    assert_eq!(cpu.known_count, vec![3]);
    assert!(
        db.read(|c| fetch(
            c,
            &Fetch {
                after: Some(down.cursor + 1),
                ..request.clone()
            },
            settings
        ))
        .is_err()
    );
}
#[test]
fn metrics_columnar_bounds_and_range_queries_use_primary_indexes() {
    let (_root, db) = database();
    let mut request = fetch_request(0, DAY, Resolution::Auto, 20);
    request.series = vec!["tps".into(); 9];
    assert!(!request.valid());
    request.series = vec!["tps".into(), "tps".into()];
    assert!(!request.valid());
    request.series = vec!["tps".into()];
    request.max_points = MAX_POINTS + 1;
    assert!(!request.valid());
    request.max_points = 20;
    request.resolution = Resolution::Raw;
    request.to = 8 * DAY;
    assert!(!request.valid());
    db.read(|c| {
        for sql in [
            "EXPLAIN QUERY PLAN SELECT sampled_at,tps FROM metrics_raw WHERE sampled_at>=0 AND sampled_at<=100 AND revision>0 ORDER BY sampled_at",
            "EXPLAIN QUERY PLAN SELECT bucket_start,data FROM metrics_5m WHERE bucket_start>=0 AND bucket_start<=100 AND revision>0 ORDER BY bucket_start",
            "EXPLAIN QUERY PLAN SELECT bucket_start,data FROM metrics_daily WHERE bucket_start>=0 AND bucket_start<=100 AND revision>0 ORDER BY bucket_start",
            "EXPLAIN QUERY PLAN WITH observed AS (SELECT *,sampled_at-LAG(sampled_at) OVER (ORDER BY sampled_at) AS interval_gap FROM metrics_raw WHERE sampled_at>=0 AND sampled_at<=100) SELECT AVG(tps),MAX(interval_gap) FROM observed GROUP BY sampled_at/100",
        ] {
            let plans=c.prepare(sql)?.query_map([],|r|r.get::<_,String>(3))?.collect::<std::result::Result<Vec<_>,_>>()?;
            assert!(plans.iter().any(|p|p.contains("SEARCH")&&p.contains("INTEGER PRIMARY KEY")),"{plans:?}");
        }Ok(())
    }).unwrap();
}
#[test]
fn metrics_shrinking_retention_prunes_in_fixed_batches_and_marks_incremental_gap() {
    let (_root, db) = database();
    let now = 100 * DAY;
    db.write(|c| {
        for n in 0..10_000 {c.execute("INSERT INTO metrics_raw(sampled_at,phase,readiness,bytes,revision) VALUES(?,'stopped','unknown',1,1)",[n])?;}
        c.execute("UPDATE metrics_status SET storage_bytes=10000,revision=1 WHERE singleton=1",[])?;Ok(())
    }).unwrap();
    let settings = MetricsSettings {
        raw_retention_hours: 1,
        ..MetricsSettings::default()
    };
    db.write(|c| storage::record(c, None, &[], now, settings))
        .unwrap();
    let count = db
        .read(|c| {
            Ok(c.query_row("SELECT COUNT(*) FROM metrics_raw", [], |r| {
                r.get::<_, i64>(0)
            })?)
        })
        .unwrap();
    assert_eq!(count, 10_000 - 512);
    let fetched = db
        .read(|c| {
            fetch(
                c,
                &Fetch {
                    after: Some(1),
                    ..fetch_request(0, 10_000, Resolution::Raw, 20)
                },
                settings,
            )
        })
        .unwrap();
    assert!(fetched.retention_gap);
    db.write(|c| storage::record(c, None, &[], now, settings))
        .unwrap();
    assert_eq!(db.read(storage::current).unwrap().status.pruned_rows, 1024);
    let cursor = db.read(storage::current).unwrap().status.revision;
    // A later publication with no deletions must not perpetually force refreshes.
    db.write(|c| {
        c.execute(
            "UPDATE metrics_status SET revision=revision+1 WHERE singleton=1",
            [],
        )?;
        Ok(())
    })
    .unwrap();
    let later = db
        .read(|c| {
            fetch(
                c,
                &Fetch {
                    after: Some(cursor),
                    ..fetch_request(0, 10_000, Resolution::Raw, 20)
                },
                settings,
            )
        })
        .unwrap();
    assert!(!later.retention_gap);
}
#[test]
fn metrics_policy_document_bounds_and_tick_poll_consent_are_explicit() {
    let mut policy = PolicyDocument::host(MetricsSettings::default());
    assert!(policy.valid());
    assert!(!policy.command_polling.enabled);
    assert_eq!(policy.command_polling.interval_seconds, 60);
    policy.settings.raw_retention_hours = 168;
    policy.settings.five_minute_retention_days = 365;
    policy.settings.daily_retention_days = 1825;
    policy.settings.max_storage_bytes = 8 * 1024 * 1024 * 1024;
    assert!(policy.valid());
    policy.command_polling.interval_seconds = 29;
    assert!(!policy.valid());
    policy.command_polling.interval_seconds = 30;
    assert!(policy.valid());
    policy.command_polling.interval_seconds = 3601;
    assert!(!policy.valid());
}
