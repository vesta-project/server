//! Same-transaction incremental buckets: no replay, unbounded catch-up, or empty buckets.
use super::*;
use crate::config::MetricsSettings;
use rusqlite::{Connection, OptionalExtension, params, params_from_iter};
use std::collections::BTreeSet;
use vesta_state::{Result, StateError};
const MAX_NAMES: usize = 10_000;
const BATCH: i64 = 512;
fn invalid() -> StateError {
    StateError::new("state_metrics_invalid_request")
}
fn decode<T: for<'de> Deserialize<'de>>(value: &str) -> Result<T> {
    serde_json::from_str(value).map_err(|_| StateError::new("state_metrics_unavailable"))
}
fn encode<T: Serialize>(value: &T) -> Result<String> {
    serde_json::to_string(value).map_err(|_| StateError::new("state_metrics_unavailable"))
}
fn account(c: &Connection, delta: i64) -> Result<()> {
    c.execute(
        "UPDATE metrics_status SET storage_bytes=storage_bytes+? WHERE singleton=1",
        [delta],
    )?;
    Ok(())
}
fn bucket(
    c: &Connection,
    table: &str,
    at: i64,
    sample: Option<&Sample>,
    activity: &[Activity],
) -> Result<()> {
    let old: Option<(String, String, bool, i64)> = c
        .query_row(
            &format!(
                "SELECT data,player_names,names_complete,bytes FROM {table} WHERE bucket_start=?"
            ),
            [at],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()?;
    let (mut accumulator, mut names, mut complete, old_bytes) = match old {
        Some((data, names, complete, bytes)) => (
            decode::<Accumulator>(&data)?,
            decode::<BTreeSet<String>>(&names)?,
            complete,
            bytes,
        ),
        None => (Accumulator::default(), BTreeSet::new(), true, 0),
    };
    if let Some(sample) = sample {
        accumulator.sample(sample);
    }
    for event in activity {
        match event.kind.as_str() {
            "join" => {
                accumulator.joins += 1;
                if table == "metrics_daily"
                    && let Some(name) = &event.name
                {
                    let name = name.to_ascii_lowercase();
                    if names.contains(&name) || names.len() < MAX_NAMES {
                        names.insert(name);
                    } else {
                        complete = false;
                    }
                }
            }
            "leave" => accumulator.leaves += 1,
            _ => {
                accumulator.gaps += 1;
                complete = false;
            }
        }
    }
    let data = encode(&accumulator)?;
    let names = encode(&names)?;
    let bytes = (data.len() + names.len() + 64) as i64;
    c.execute(&format!("INSERT INTO {table}(bucket_start,data,player_names,names_complete,bytes) VALUES(?,?,?,?,?) ON CONFLICT(bucket_start) DO UPDATE SET data=excluded.data,player_names=excluded.player_names,names_complete=excluded.names_complete,bytes=excluded.bytes"), params![at,data,names,complete,bytes])?;
    account(c, bytes - old_bytes)
}

pub(super) fn record(
    c: &Connection,
    sample: Option<&Sample>,
    activity: &[Activity],
    at: i64,
    settings: MetricsSettings,
) -> Result<()> {
    let mut events = activity.to_vec();
    let mut accepted = sample;
    if let Some(sample) = sample {
        let last: Option<i64> = c.query_row(
            "SELECT last_at FROM metrics_status WHERE singleton=1",
            [],
            |r| r.get(0),
        )?;
        if let Some(last) = last
            && sample.sampled_at <= last
        {
            if last.saturating_sub(sample.sampled_at) <= 600_000 {
                c.execute(
                    "UPDATE metrics_status SET clock_skips=clock_skips+1 WHERE singleton=1",
                    [],
                )?;
                accepted = None;
            } else {
                c.execute(
                    "UPDATE metrics_status SET clock_steps=clock_steps+1 WHERE singleton=1",
                    [],
                )?;
                events.push(Activity {
                    id: 0,
                    at,
                    generation: sample.generation.clone(),
                    name: None,
                    kind: "clock_step".into(),
                });
            }
        }
    }
    if let Some(sample) = accepted {
        let columns = SERIES.join(",");
        let placeholders = (0..SERIES.len() + 5)
            .map(|_| "?")
            .collect::<Vec<_>>()
            .join(",");
        let latest_json = encode(sample)?;
        let bytes = latest_json.len() as i64 + 64;
        let mut values = vec![
            rusqlite::types::Value::Integer(sample.sampled_at),
            sample
                .generation
                .clone()
                .map_or(rusqlite::types::Value::Null, Into::into),
            sample.phase.clone().into(),
            sample.readiness.clone().into(),
        ];
        values.extend(SERIES.map(|name| {
            sample.values[name].map_or(rusqlite::types::Value::Null, rusqlite::types::Value::Real)
        }));
        values.push(bytes.into());
        let inserted = c.execute(&format!("INSERT OR IGNORE INTO metrics_raw(sampled_at,generation,phase,readiness,{columns},bytes) VALUES({placeholders})"),params_from_iter(values))?;
        if inserted != 0 {
            let old_len:i64=c.query_row("SELECT COALESCE(length(CAST(latest_json AS BLOB)),0) FROM metrics_status WHERE singleton=1",[],|r|r.get(0))?;
            account(c, bytes + latest_json.len() as i64 - old_len)?;
            c.execute(
                "UPDATE metrics_status SET last_at=?,latest_json=? WHERE singleton=1",
                params![sample.sampled_at, latest_json],
            )?;
        } else {
            accepted = None;
        }
    }
    // Every event is inserted once by the owned console cursor. A lost transaction
    // is exposed as a gap by the sampler before it advances that cursor again.
    for event in &events {
        let bytes = (event.name.as_ref().map_or(0, |n| n.len())
            + event.generation.as_ref().map_or(0, |g| g.len())
            + 64) as i64;
        c.execute(
            "INSERT INTO metrics_activity(at,generation,name,kind,bytes) VALUES(?,?,?,?,?)",
            params![event.at, event.generation, event.name, event.kind, bytes],
        )?;
        account(c, bytes)?;
    }
    for (table, width) in [("metrics_5m", 300_000), ("metrics_daily", DAY)] {
        let mut buckets: BTreeSet<i64> = events
            .iter()
            .map(|e| e.at.div_euclid(width) * width)
            .collect();
        if let Some(sample) = accepted {
            buckets.insert(sample.sampled_at.div_euclid(width) * width);
        }
        for start in buckets {
            let relevant: Vec<_> = events
                .iter()
                .filter(|e| e.at.div_euclid(width) * width == start)
                .cloned()
                .collect();
            bucket(
                c,
                table,
                start,
                accepted.filter(|s| s.sampled_at.div_euclid(width) * width == start),
                &relevant,
            )?;
        }
    }
    cleanup(c, at, settings)
}
fn prune(c: &Connection, table: &str, key: &str, cutoff: Option<i64>) -> Result<i64> {
    let filter = cutoff.map_or(String::new(), |_| format!("WHERE {key} < ?"));
    let sql = format!("SELECT {key},bytes FROM {table} {filter} ORDER BY {key} LIMIT {BATCH}");
    let mut query = c.prepare(&sql)?;
    let args: Vec<_> = cutoff.into_iter().collect();
    let selected: Vec<(i64, i64)> = query
        .query_map(params_from_iter(args), |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<std::result::Result<_, _>>()?;
    let bytes: i64 = selected.iter().map(|(_, bytes)| bytes).sum();
    for (id, _) in &selected {
        c.execute(&format!("DELETE FROM {table} WHERE {key}=?"), [id])?;
    }
    if !selected.is_empty() {
        account(c, -bytes)?;
        c.execute("UPDATE metrics_status SET pruned_rows=pruned_rows+?,pruned_bytes=pruned_bytes+?,activity_pruned=MAX(activity_pruned,?) WHERE singleton=1",params![selected.len() as i64,bytes,table=="metrics_activity"])?;
    }
    Ok(selected.len() as i64)
}
fn cleanup(c: &Connection, now: i64, settings: MetricsSettings) -> Result<()> {
    // Fixed work bound even after reducing configuration or a long outage. Large
    // preexisting excesses converge over ticks, and status exposes the excess.
    for (table, key, keep) in [
        (
            "metrics_raw",
            "sampled_at",
            settings.raw_retention_hours as i64 * 3_600_000,
        ),
        (
            "metrics_5m",
            "bucket_start",
            settings.five_minute_retention_days as i64 * DAY,
        ),
        (
            "metrics_daily",
            "bucket_start",
            settings.daily_retention_days as i64 * DAY,
        ),
        (
            "metrics_activity",
            "id",
            settings.five_minute_retention_days as i64 * DAY,
        ),
    ] {
        if table == "metrics_activity" {
            // IDs need not have increasing wall time after a clock correction.
            for _ in 0..4 {
                let ids: Vec<(i64, i64)> = c
                    .prepare(
                        "SELECT id,bytes FROM metrics_activity WHERE at<? ORDER BY at,id LIMIT 512",
                    )?
                    .query_map([now.saturating_sub(keep)], |r| Ok((r.get(0)?, r.get(1)?)))?
                    .collect::<std::result::Result<_, _>>()?;
                if ids.is_empty() {
                    break;
                }
                let bytes: i64 = ids.iter().map(|(_, b)| b).sum();
                for (id, _) in &ids {
                    c.execute("DELETE FROM metrics_activity WHERE id=?", [id])?;
                }
                account(c, -bytes)?;
                c.execute("UPDATE metrics_status SET pruned_rows=pruned_rows+?,pruned_bytes=pruned_bytes+?,activity_pruned=1 WHERE singleton=1",params![ids.len() as i64,bytes])?;
            }
        } else {
            for _ in 0..4 {
                if prune(c, table, key, Some(now.saturating_sub(keep)))? < BATCH {
                    break;
                }
            }
        }
    }
    for (table, key) in [
        ("metrics_raw", "sampled_at"),
        ("metrics_5m", "bucket_start"),
        ("metrics_daily", "bucket_start"),
        ("metrics_activity", "id"),
    ] {
        for _ in 0..16 {
            let size: i64 = c.query_row(
                "SELECT storage_bytes FROM metrics_status WHERE singleton=1",
                [],
                |r| r.get(0),
            )?;
            if size <= settings.max_storage_bytes as i64 {
                return Ok(());
            }
            if prune(c, table, key, None)? == 0 {
                break;
            }
        }
        // Do not prune a later tier before exhausting older raw/aggregate rows.
        let remains: bool =
            c.query_row(&format!("SELECT EXISTS(SELECT 1 FROM {table})"), [], |r| {
                r.get(0)
            })?;
        if remains {
            break;
        }
    }
    Ok(())
}

pub(crate) fn current(c: &Connection) -> Result<Current> {
    let status=c.query_row("SELECT last_at,storage_bytes,clock_skips,clock_steps,pruned_rows,pruned_bytes,activity_pruned FROM metrics_status WHERE singleton=1",[],|r|Ok(Status{last_at:r.get(0)?,storage_bytes:r.get(1)?,clock_skips:r.get(2)?,clock_steps:r.get(3)?,pruned_rows:r.get(4)?,pruned_bytes:r.get(5)?,activity_pruned:r.get(6)?}))?;
    let latest: Option<String> = c.query_row(
        "SELECT latest_json FROM metrics_status WHERE singleton=1",
        [],
        |r| r.get(0),
    )?;
    let sample = latest.map(|v| decode::<Sample>(&v)).transpose()?;
    Ok(Current { sample, status })
}
pub(crate) fn range(
    c: &Connection,
    series: &str,
    from: i64,
    to: i64,
    resolution: Resolution,
) -> Result<Range> {
    if !SERIES.contains(&series) || from < 0 || to < from || to.saturating_sub(from) > 730 * DAY {
        return Err(invalid());
    }
    let resolution = resolution.select(from, to);
    let mut points = Vec::new();
    if resolution == Resolution::Raw {
        let mut statement=c.prepare(&format!("SELECT sampled_at,{series} FROM metrics_raw WHERE sampled_at>=? AND sampled_at<=? ORDER BY sampled_at LIMIT 2001"))?;
        for row in statement.query_map(params![from, to], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, Option<f64>>(1)?))
        })? {
            let (at, value) = row?;
            let mut numeric = Numeric::default();
            numeric.add(value);
            points.push(Point {
                at,
                numeric,
                joins: 0,
                leaves: 0,
                gaps: 0,
                unique_joining_players: None,
                peak_online: None,
            });
        }
    } else {
        let table = if resolution == Resolution::FiveMinute {
            "metrics_5m"
        } else {
            "metrics_daily"
        };
        let mut statement=c.prepare(&format!("SELECT bucket_start,data,player_names,names_complete FROM {table} WHERE bucket_start>=? AND bucket_start<=? ORDER BY bucket_start LIMIT 2001"))?;
        for row in statement.query_map(params![from, to], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, bool>(3)?,
            ))
        })? {
            let (at, data, names, complete) = row?;
            let data: Accumulator = decode(&data)?;
            let numeric = data.series.get(series).copied().unwrap_or_default();
            let unique_joining_players = if resolution == Resolution::Daily && complete {
                Some(decode::<BTreeSet<String>>(&names)?.len())
            } else {
                None
            };
            points.push(Point {
                at,
                numeric,
                joins: data.joins,
                leaves: data.leaves,
                gaps: data.gaps,
                unique_joining_players,
                peak_online: data.series.get("players_online").and_then(|n| n.max),
            });
        }
    }
    if points.len() > MAX_POINTS {
        return Err(invalid());
    }
    Ok(Range {
        series: series.into(),
        resolution,
        points,
    })
}
pub(crate) fn activity(c: &Connection, after: i64, limit: usize) -> Result<Vec<Activity>> {
    if after < 0 || !(1..=101).contains(&limit) {
        return Err(invalid());
    }
    Ok(c.prepare(
        "SELECT id,at,generation,name,kind FROM metrics_activity WHERE id>? ORDER BY id LIMIT ?",
    )?
    .query_map(params![after, limit as i64], |r| {
        Ok(Activity {
            id: r.get(0)?,
            at: r.get(1)?,
            generation: r.get(2)?,
            name: r.get(3)?,
            kind: r.get(4)?,
        })
    })?
    .collect::<std::result::Result<_, _>>()?)
}
