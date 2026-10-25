//! Local bounded telemetry. Unknown values stay NULL; no console commands are sent.
mod fetch;
pub(crate) use fetch::fetch;
pub use fetch::{Columnar, Fetch, Fetched, MAX_SERIES};
mod native;
mod network;
mod policy;
pub(crate) mod ticks;
pub use policy::{CommandPolling, Document as PolicyDocument, Policy};
pub(crate) use policy::{effective as effective_policy, get as get_policy};
mod sampler;
pub(crate) mod storage;
pub(crate) use sampler::Sampler;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const SERIES: [&str; 17] = [
    "game_cpu_percent",
    "game_memory_bytes",
    "agent_cpu_percent",
    "agent_memory_bytes",
    "host_cpu_percent",
    "host_memory_used_bytes",
    "host_memory_total_bytes",
    "game_disk_free_bytes",
    "backup_disk_free_bytes",
    "players_online",
    "players_max",
    "host_network_received_bytes",
    "host_network_sent_bytes",
    "tps",
    "mspt",
    "game_network_received_bytes",
    "game_network_sent_bytes",
];
pub const MAX_POINTS: usize = 2000;
pub(crate) const DAY: i64 = 86_400_000;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Sample {
    pub sampled_at: i64,
    pub generation: Option<String>,
    pub phase: String,
    pub readiness: String,
    pub values: BTreeMap<String, Option<f64>>,
    pub unknown: Vec<String>,
    #[serde(default)]
    pub tick_source: Option<String>,
    #[serde(default)]
    pub tick_reason: Option<String>,
}
impl Sample {
    pub(crate) fn new(
        at: i64,
        generation: Option<String>,
        phase: String,
        readiness: String,
        values: [Option<f64>; 17],
    ) -> Self {
        let values: BTreeMap<_, _> = SERIES
            .into_iter()
            .zip(values)
            .map(|(name, v)| (name.to_owned(), v.filter(|v| v.is_finite() && *v >= 0.0)))
            .collect();
        let unknown = values
            .iter()
            .filter(|(_, v)| v.is_none())
            .map(|(name, _)| name.clone())
            .collect();
        Self {
            sampled_at: at,
            generation,
            phase,
            readiness,
            values,
            unknown,
            tick_source: None,
            tick_reason: None,
        }
    }
}
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize)]
pub struct Numeric {
    pub min: Option<f64>,
    pub avg: Option<f64>,
    pub max: Option<f64>,
    pub known_count: u64,
    pub unknown_count: u64,
    #[serde(skip)]
    pub(crate) sum: f64,
}
impl Numeric {
    fn add(&mut self, value: Option<f64>) {
        if let Some(v) = value {
            self.known_count += 1;
            self.sum += v;
            self.min = Some(self.min.map_or(v, |old| old.min(v)));
            self.max = Some(self.max.map_or(v, |old| old.max(v)));
            self.avg = Some(self.sum / self.known_count as f64);
        } else {
            self.unknown_count += 1;
        }
    }
}
// Storage carries sum, whereas public numeric projections omit it.
#[derive(Clone, Default, Deserialize, Serialize)]
pub(crate) struct Accumulator {
    pub series: BTreeMap<String, Numeric>,
    pub sums: BTreeMap<String, f64>,
    pub joins: u64,
    pub leaves: u64,
    pub gaps: u64,
    #[serde(default)]
    pub tick_source: Option<String>,
}
impl Accumulator {
    fn sample(&mut self, sample: &Sample) {
        if self.series.is_empty() {
            self.tick_source = sample.tick_source.clone();
        } else if self.tick_source != sample.tick_source {
            self.tick_source = None;
        }
        for name in SERIES {
            let numeric = self.series.entry(name.to_owned()).or_default();
            numeric.sum = self.sums.get(name).copied().unwrap_or(0.0);
            numeric.add(sample.values[name]);
            self.sums.insert(name.to_owned(), numeric.sum);
        }
    }
}
#[derive(Serialize)]
pub struct Point {
    pub at: i64,
    pub numeric: Numeric,
    pub joins: u64,
    pub leaves: u64,
    pub gaps: u64,
    pub unique_joining_players: Option<usize>,
    pub peak_online: Option<f64>,
}
#[derive(Serialize)]
pub struct Range {
    pub series: String,
    pub resolution: Resolution,
    pub points: Vec<Point>,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Resolution {
    Auto,
    Raw,
    #[serde(rename = "5m")]
    FiveMinute,
    Daily,
}
impl Resolution {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "auto" => Some(Self::Auto),
            "raw" => Some(Self::Raw),
            "5m" => Some(Self::FiveMinute),
            "daily" => Some(Self::Daily),
            _ => None,
        }
    }
    pub(crate) fn select(self, from: i64, to: i64) -> Self {
        if self != Self::Auto {
            return self;
        }
        match to - from {
            span if span <= 2 * 3_600_000 => Self::Raw,
            span if span <= 6 * DAY => Self::FiveMinute,
            _ => Self::Daily,
        }
    }
}
#[derive(Clone, Debug, Serialize)]
pub struct Activity {
    pub id: i64,
    pub at: i64,
    pub generation: Option<String>,
    pub name: Option<String>,
    pub kind: String,
}
#[derive(Default, Serialize)]
pub struct Status {
    pub last_at: Option<i64>,
    pub storage_bytes: i64,
    pub clock_skips: i64,
    pub clock_steps: i64,
    pub pruned_rows: i64,
    pub pruned_bytes: i64,
    pub activity_pruned: bool,
    pub revision: i64,
}
#[derive(Serialize)]
pub struct Current {
    pub sample: Option<Sample>,
    pub columns: Columnar,
    pub status: Status,
}

pub(crate) fn activity_line(line: &str) -> Option<(&str, &'static str)> {
    // The same anchored INFO envelopes as readiness: vanilla server-thread lines and
    // the Paper-family console form. Chat, other threads and substrings never match.
    let message = vesta_game_runtime::readiness::server_info_message(line)?;
    let message = message.strip_prefix("System chat: ").unwrap_or(message);
    let (name, kind) = if let Some(name) = message.strip_suffix(" joined the game") {
        (name, "join")
    } else if let Some(name) = login_name(message) {
        (name, "join")
    } else if let Some((name, reason)) = message.split_once(" lost connection: ") {
        if reason.is_empty() {
            return None;
        }
        (name, "leave")
    } else {
        (message.strip_suffix(" left the game")?, "leave")
    };
    crate::moderation::valid_player(name).then_some((name, kind))
}

/// A shared Minecraft/Bukkit-family message shape, independent of software identity.
/// Validate the whole message so chat and partial login diagnostics cannot become joins.
fn login_name(message: &str) -> Option<&str> {
    let (name, tail) = message.split_once('[')?;
    if !crate::moderation::valid_player(name) {
        return None;
    }
    let (address, tail) = tail.split_once("] logged in with entity id ")?;
    address
        .strip_prefix('/')?
        .parse::<std::net::SocketAddr>()
        .ok()?;
    let (entity, position) = tail.split_once(" at (")?;
    entity.parse::<i32>().ok()?;
    let mut position = position.strip_suffix(')')?;
    if let Some(world) = position.strip_prefix('[') {
        let (world, coordinates) = world.split_once(']')?;
        if world.is_empty() || world.len() > 256 || world.chars().any(char::is_control) {
            return None;
        }
        position = coordinates;
    }
    let mut coordinates = position.split(',');
    for _ in 0..3 {
        if !coordinates.next()?.trim().parse::<f64>().ok()?.is_finite() {
            return None;
        }
    }
    if coordinates.next().is_some() {
        return None;
    }
    Some(name)
}

/// Consecutive login/join and disconnect/leave lines describe one transition.
/// Bounded independently of console retention, and reset at generation/stream gaps.
#[derive(Default)]
struct ActivityTransitions(std::collections::BTreeMap<String, &'static str>);
impl ActivityTransitions {
    fn clear(&mut self) {
        self.0.clear();
    }
    fn observe(&mut self, name: &str, kind: &'static str) -> Option<bool> {
        if self.0.get(name).copied() == Some(kind) {
            return Some(false);
        }
        if !self.0.contains_key(name) && self.0.len() >= 10_000 {
            return None;
        }
        self.0.insert(name.to_owned(), kind);
        Some(true)
    }
}

#[cfg(test)]
mod tests;
