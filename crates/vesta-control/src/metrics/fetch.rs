//! Indexed, bounded columnar dashboard projections. Cursors are database revisions,
//! not wall time: a newly updated 5-minute bucket is visible to incremental clients.
use super::*;
use crate::config::MetricsSettings;
use rusqlite::{Connection, params};
use vesta_state::{Result, StateError};

pub const MAX_SERIES: usize = 8;
#[derive(Clone, Debug)]
pub struct Fetch {
    pub series: Vec<String>,
    pub from: i64,
    pub to: i64,
    pub resolution: Resolution,
    pub max_points: usize,
    pub after: Option<i64>,
}
impl Fetch {
    pub fn valid(&self) -> bool {
        let names: std::collections::BTreeSet<_> = self.series.iter().collect();
        !self.series.is_empty()
            && self.series.len() <= MAX_SERIES
            && names.len() == self.series.len()
            && self.series.iter().all(|s| SERIES.contains(&s.as_str()))
            && self.from >= 0
            && self.to >= self.from
            && self.to - self.from <= 1825 * DAY
            && (1..=MAX_POINTS).contains(&self.max_points)
            && self.after.is_none_or(|a| a >= 0)
            && (self.resolution != Resolution::Raw || self.to - self.from <= 7 * DAY)
    }
}
#[derive(Default, Serialize)]
pub struct Column {
    pub avg: Vec<Option<f64>>,
    pub min: Vec<Option<f64>>,
    pub max: Vec<Option<f64>>,
    pub known_count: Vec<u64>,
    pub unknown_count: Vec<u64>,
}
impl Column {
    fn push(&mut self, n: Numeric) {
        self.avg.push(n.avg);
        self.min.push(n.min);
        self.max.push(n.max);
        self.known_count.push(n.known_count);
        self.unknown_count.push(n.unknown_count);
    }
}
#[derive(Serialize)]
pub struct Columnar {
    pub timestamps: Vec<i64>,
    pub series: BTreeMap<String, Column>,
    pub gaps: Vec<bool>,
    pub tick_source: Vec<Option<String>>,
}
impl Columnar {
    pub fn sample(sample: Option<&Sample>) -> Self {
        let mut result = Self {
            timestamps: Vec::new(),
            series: SERIES
                .into_iter()
                .map(|s| (s.into(), Column::default()))
                .collect(),
            gaps: Vec::new(),
            tick_source: Vec::new(),
        };
        if let Some(sample) = sample {
            result.timestamps.push(sample.sampled_at);
            result.gaps.push(false);
            result.tick_source.push(sample.tick_source.clone());
            for name in SERIES {
                let mut n = Numeric::default();
                n.add(sample.values[name]);
                result.series.get_mut(name).expect("closed series").push(n);
            }
        }
        result
    }
}
#[derive(Serialize)]
pub struct Fetched {
    #[serde(flatten)]
    pub columns: Columnar,
    pub resolution: Resolution,
    pub bucket_millis: i64,
    pub cursor: i64,
    /// Incremental callers must refresh their full window after pruning. No
    /// tombstone log is kept; this explicit marker makes missing data honest.
    pub retention_gap: bool,
    #[serde(skip)]
    pub unchanged: bool,
}
fn invalid() -> StateError {
    StateError::new("state_metrics_invalid_request")
}
#[derive(Default)]
struct Bucket {
    values: BTreeMap<String, Numeric>,
    gaps: bool,
    source: Option<String>,
    source_initialized: bool,
}
impl Bucket {
    fn add(&mut self, name: &str, value: Numeric) {
        let entry = self.values.entry(name.into()).or_default();
        entry.sum += value.avg.unwrap_or(0.0) * value.known_count as f64;
        entry.known_count += value.known_count;
        entry.unknown_count += value.unknown_count;
        if let Some(min) = value.min {
            entry.min = Some(entry.min.map_or(min, |v| v.min(min)));
        }
        if let Some(max) = value.max {
            entry.max = Some(entry.max.map_or(max, |v| v.max(max)));
        }
        entry.avg = (entry.known_count > 0).then(|| entry.sum / entry.known_count as f64);
    }
    fn source(&mut self, source: Option<String>) {
        if !self.source_initialized {
            self.source = source;
            self.source_initialized = true;
        } else if self.source != source {
            self.source = None;
        }
    }
}
fn selected(c: &Connection, request: &Fetch, settings: MetricsSettings) -> Result<Resolution> {
    if request.resolution != Resolution::Auto {
        return Ok(request.resolution);
    }
    let latest: Option<i64> = c.query_row(
        "SELECT last_at FROM metrics_status WHERE singleton=1",
        [],
        |r| r.get(0),
    )?;
    let latest = latest.unwrap_or(request.to);
    let span = request.to - request.from;
    if span <= 7 * DAY
        && request.from >= latest.saturating_sub(settings.raw_retention_hours as i64 * 3_600_000)
        && span / (settings.sample_seconds as i64 * 1000) < request.max_points as i64
    {
        Ok(Resolution::Raw)
    } else if request.from
        >= latest.saturating_sub(settings.five_minute_retention_days as i64 * DAY)
        && span / 300_000 < request.max_points as i64
    {
        Ok(Resolution::FiveMinute)
    } else {
        Ok(Resolution::Daily)
    }
}

pub(crate) fn fetch(c: &Connection, request: &Fetch, settings: MetricsSettings) -> Result<Fetched> {
    if !request.valid() {
        return Err(invalid());
    }
    let cursor: i64 = c.query_row(
        "SELECT revision FROM metrics_status WHERE singleton=1",
        [],
        |r| r.get(0),
    )?;
    if request.after.is_some_and(|a| a > cursor) {
        return Err(invalid());
    }
    let resolution = selected(c, request, settings)?;
    let base = match resolution {
        Resolution::Raw => settings.sample_seconds as i64 * 1000,
        Resolution::FiveMinute => 300_000,
        _ => DAY,
    };
    let span = request.to - request.from;
    // Ceil to an integral source bucket multiple and include both endpoints.
    let needed = span / request.max_points as i64 + 1;
    let width = ((needed + base - 1) / base).max(1) * base;
    if request.after == Some(cursor) {
        return Ok(Fetched {
            columns: Columnar {
                timestamps: Vec::new(),
                series: request
                    .series
                    .iter()
                    .map(|n| (n.clone(), Column::default()))
                    .collect(),
                gaps: Vec::new(),
                tick_source: Vec::new(),
            },
            resolution,
            bucket_millis: width,
            cursor,
            retention_gap: false,
            unchanged: true,
        });
    }
    let mut buckets = BTreeMap::<i64, Bucket>::new();
    if resolution == Resolution::Raw {
        // Aggregate in SQLite: at most max_points rows cross the Rust boundary.
        // The timestamp primary key supplies the bounded range seek.
        let numeric = request
            .series
            .iter()
            .map(|name| {
                format!("AVG({name}),MIN({name}),MAX({name}),COUNT({name}),COUNT(*)-COUNT({name})")
            })
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "WITH observed AS (SELECT *,sampled_at-LAG(sampled_at) OVER (ORDER BY sampled_at) AS interval_gap FROM metrics_raw WHERE sampled_at>=?1 AND sampled_at<=?3) SELECT ((sampled_at-?1)/?2)*?2+?1,{numeric},MIN(sampled_at),MAX(sampled_at),COUNT(*),CASE WHEN COUNT(tick_source)=COUNT(*) AND MIN(tick_source)=MAX(tick_source) THEN MIN(tick_source) ELSE NULL END,MAX(interval_gap) FROM observed GROUP BY (sampled_at-?1)/?2 HAVING MAX(revision)>?4 ORDER BY 1 LIMIT ?5"
        );
        let mut query = c.prepare(&sql)?;
        let mut rows = query.query(params![
            request.from,
            width,
            request.to,
            request.after.unwrap_or(-1),
            request.max_points as i64 + 1
        ])?;
        let mut previous = None;
        while let Some(row) = rows.next()? {
            let at: i64 = row.get(0)?;
            let mut bucket = Bucket::default();
            for (i, name) in request.series.iter().enumerate() {
                let start = 1 + i * 5;
                bucket.add(
                    name,
                    Numeric {
                        avg: row.get(start)?,
                        min: row.get(start + 1)?,
                        max: row.get(start + 2)?,
                        known_count: row.get::<_, i64>(start + 3)? as u64,
                        unknown_count: row.get::<_, i64>(start + 4)? as u64,
                        sum: 0.0,
                    },
                );
            }
            let index = 1 + request.series.len() * 5;
            let first: i64 = row.get(index)?;
            let last: i64 = row.get(index + 1)?;
            let count: i64 = row.get(index + 2)?;
            let max_gap: Option<i64> = row.get(index + 4)?;
            let marker:bool=c.query_row("SELECT EXISTS(SELECT 1 FROM metrics_activity WHERE at>=? AND at<? AND kind NOT IN ('join','leave'))",params![at,at.saturating_add(width)],|r|r.get(0))?;
            bucket.gaps = marker
                || max_gap.is_some_and(|gap| gap > base * 2)
                || previous.is_some_and(|before| first - before > base * 2)
                || last - first > count * base * 2;
            bucket.source(row.get(index + 3)?);
            previous = Some(last);
            buckets.insert(at, bucket);
        }
    } else {
        let table = if resolution == Resolution::FiveMinute {
            "metrics_5m"
        } else {
            "metrics_daily"
        };
        let sql = format!(
            "SELECT bucket_start,data,revision FROM {table} WHERE bucket_start>=? AND bucket_start<=? ORDER BY bucket_start"
        );
        let mut query = c.prepare(&sql)?;
        let mut rows = query.query(params![request.from, request.to])?;
        let mut changed = std::collections::BTreeSet::new();
        let mut previous = None;
        // At most 105,121 5-minute buckets for the policy's one-year ceiling,
        // or 1,826 daily buckets. No per-point JSON is returned to clients.
        let mut scanned = 0;
        while let Some(row) = rows.next()? {
            scanned += 1;
            if scanned > 105_408 {
                return Err(invalid());
            }
            let at: i64 = row.get(0)?;
            let json: String = row.get(1)?;
            let data: Accumulator = serde_json::from_str(&json)
                .map_err(|_| StateError::new("state_metrics_unavailable"))?;
            let start = (at - request.from) / width * width + request.from;
            let bucket = buckets.entry(start).or_default();
            if row.get::<_, i64>(2)? > request.after.unwrap_or(-1) {
                changed.insert(start);
            }
            bucket.gaps |= data.gaps > 0 || previous.is_some_and(|before| at - before > base);
            previous = Some(at);
            bucket.source(data.tick_source);
            for name in &request.series {
                bucket.add(name, data.series.get(name).copied().unwrap_or_default());
            }
        }
        buckets.retain(|at, _| changed.contains(at));
    }
    if buckets.len() > request.max_points {
        return Err(invalid());
    }
    let mut columns = Columnar {
        timestamps: Vec::new(),
        series: request
            .series
            .iter()
            .map(|n| (n.clone(), Column::default()))
            .collect(),
        gaps: Vec::new(),
        tick_source: Vec::new(),
    };
    for (at, bucket) in buckets {
        columns.timestamps.push(at);
        columns.gaps.push(bucket.gaps);
        columns.tick_source.push(bucket.source);
        for name in &request.series {
            columns
                .series
                .get_mut(name)
                .expect("validated series")
                .push(bucket.values.get(name).copied().unwrap_or_default());
        }
    }
    let prune_revision: i64 = c.query_row(
        "SELECT prune_revision FROM metrics_status WHERE singleton=1",
        [],
        |r| r.get(0),
    )?;
    Ok(Fetched {
        columns,
        resolution,
        bucket_millis: width,
        cursor,
        retention_gap: request.after.is_some_and(|after| after < prune_revision),
        unchanged: request.after == Some(cursor),
    })
}
