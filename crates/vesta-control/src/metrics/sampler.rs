use super::{native, storage, *};
use crate::{
    config::MetricsSettings,
    operations::{ProcessPhase, RuntimeHandle},
};
use std::{
    path::PathBuf,
    time::{Duration, Instant, UNIX_EPOCH},
};
use vesta_game_runtime::{console::MAX_PAGE_LINES, process::GenerationId};
use vesta_state::StateDatabase;

pub(crate) struct Sampler {
    db: StateDatabase,
    settings: MetricsSettings,
    host_network: super::network::Delta,
    tick_detection: super::ticks::Detection,
    tick_adapter: Option<super::ticks::Adapter>,
    tick_pending: Option<std::sync::mpsc::Receiver<Result<super::ticks::Observation, ()>>>,
    tick_next: Option<Instant>,
    tick_latest: Option<(super::ticks::Observation, Instant)>,
    tick_revision: Option<i64>,
    tick_generation: Option<GenerationId>,
    tick_reason: &'static str,
    was_enabled: bool,
    game: PathBuf,
    backup: Option<PathBuf>,
    due: Option<Instant>,
    game_cpu: native::CpuDelta,
    agent_cpu: native::CpuDelta,
    host_cpu: Option<(u64, u64)>,
    generation: Option<GenerationId>,
    cursor: u64,
    gap: bool,
    initialized: bool,
}
impl Sampler {
    pub(crate) fn new(
        db: StateDatabase,
        settings: MetricsSettings,
        game: PathBuf,
        backup: Option<PathBuf>,
    ) -> Self {
        Self {
            db,
            settings,
            host_network: super::network::Delta::default(),
            tick_detection: super::ticks::Detection::unknown(),
            tick_adapter: None,
            tick_pending: None,
            tick_next: None,
            tick_latest: None,
            tick_revision: None,
            tick_generation: None,
            tick_reason: "command polling disabled",
            was_enabled: false,
            game,
            backup,
            due: None,
            game_cpu: native::CpuDelta::default(),
            agent_cpu: native::CpuDelta::default(),
            host_cpu: None,
            generation: None,
            cursor: 0,
            gap: false,
            initialized: false,
        }
    }
    pub(crate) fn with_tick_detection(mut self, detection: super::ticks::Detection) -> Self {
        self.tick_detection = detection;
        self
    }
    pub(crate) fn tick(&mut self, runtime: &RuntimeHandle, at: i64) {
        let Ok((revision, document)) = self.db.read(|c| super::effective_policy(c, self.settings))
        else {
            // Unreadable stored policy cannot authorize command polling or sampling.
            return;
        };
        let settings = document.settings;
        let now = Instant::now();
        self.track_ticks(runtime, revision, document, now);
        if self.was_enabled != settings.enabled {
            self.host_network = super::network::Delta::default();
            self.game_cpu = native::CpuDelta::default();
            self.agent_cpu = native::CpuDelta::default();
            self.host_cpu = None;
            self.due = None;
            self.was_enabled = settings.enabled;
        }
        let due = self.due.is_none_or(|last| {
            now.duration_since(last) >= Duration::from_secs(settings.sample_seconds)
        });
        let activity = if settings.enabled {
            self.scan(runtime, at)
        } else {
            Vec::new()
        };
        if !due && activity.is_empty() {
            return;
        }
        let sample = if due && settings.enabled {
            Some(self.sample(runtime, at, now))
        } else {
            None
        };
        if due {
            self.due = Some(now);
        }
        let recorded = self.db.write(|c| {
            storage::record(c, sample.as_ref(), &activity, at, settings)?;
            c.query_row(
                "SELECT revision FROM metrics_status WHERE singleton=1",
                [],
                |r| r.get::<_, i64>(0),
            )
            .map_err(Into::into)
        });
        match recorded {
            Ok(revision) => runtime.metrics_published(revision),
            Err(_) => self.gap = true,
        }
    }
    fn track_ticks(
        &mut self,
        runtime: &RuntimeHandle,
        revision: Option<i64>,
        document: super::PolicyDocument,
        now: Instant,
    ) {
        let generation = runtime.game_snapshot().ok().and_then(|s| s.generation);
        if self.tick_revision != revision || self.tick_generation != generation {
            self.tick_revision = revision;
            self.tick_generation = generation;
            self.tick_pending = None;
            self.tick_latest = None;
            self.tick_next = None;
            self.tick_adapter = self.tick_detection.adapter();
        }
        if !document.settings.enabled || !document.command_polling.enabled || revision.is_none() {
            self.tick_pending = None;
            self.tick_latest = None;
            self.tick_next = None;
            self.tick_reason = "command polling disabled";
            return;
        }
        let Some(adapter) = self.tick_adapter else {
            self.tick_reason = self.tick_detection.reason();
            return;
        };
        let ready = runtime.readiness().is_ok_and(|r| {
            r.generation == generation
                && r.readiness == crate::operations::readiness::Readiness::Ready
        });
        if !ready || generation.is_none() {
            self.tick_pending = None;
            self.tick_latest = None;
            self.tick_reason = "game is not ready";
            return;
        }
        let interval = Duration::from_secs(document.command_polling.interval_seconds);
        if let Some(receiver) = &self.tick_pending {
            match receiver.try_recv() {
                Ok(Ok(value)) => {
                    let mut latest = self
                        .tick_latest
                        .map_or(super::ticks::Observation::default(), |(v, _)| v);
                    if value.tps.is_some() {
                        latest.tps = value.tps;
                    }
                    if value.mspt.is_some() {
                        latest.mspt = value.mspt;
                    }
                    self.tick_latest = Some((latest, now));
                    self.tick_reason = "command output observed within correlation window";
                    self.tick_pending = None;
                    self.tick_adapter = Some(adapter.next());
                }
                Ok(Err(())) | Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.tick_pending = None;
                    self.tick_latest = None;
                    self.tick_reason = "tick command timed out or output was unrecognized";
                    self.tick_next = Some(now + interval.saturating_mul(4));
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
            }
        }
        if self
            .tick_latest
            .is_some_and(|(_, at)| now.saturating_duration_since(at) > interval.saturating_mul(3))
        {
            self.tick_latest = None;
        }
        if self.tick_pending.is_none() && self.tick_next.is_none_or(|next| now >= next) {
            self.tick_pending = runtime.metrics_poll(
                generation.expect("ready generation"),
                self.tick_adapter.expect("selected adapter"),
                revision.expect("stored consent"),
            );
            self.tick_next = Some(now + interval);
            if self.tick_pending.is_none() {
                self.tick_reason = "tick command writer is busy";
            }
        }
    }
    fn scan(&mut self, runtime: &RuntimeHandle, at: i64) -> Vec<Activity> {
        let generation = runtime.game_snapshot().ok().and_then(|s| {
            s.generation
                .filter(|_| matches!(s.phase, ProcessPhase::Running | ProcessPhase::Stopping))
        });
        let mut events = Vec::new();
        if !self.initialized || generation != self.generation {
            // A boundary may have passed between ticks, including a restart while
            // the agent was down. Skip backlog rather than attribute old output.
            self.cursor = runtime
                .console_snapshot(0, 1)
                .map(|p| p.latest_cursor)
                .unwrap_or(self.cursor);
            self.generation = generation;
            self.initialized = true;
            self.gap = true;
        }
        for _ in 0..4 {
            let Ok(page) = runtime.console_snapshot(self.cursor, MAX_PAGE_LINES) else {
                self.gap = true;
                break;
            };
            if page.gap {
                self.gap = true;
            }
            let mut expected = self.cursor.saturating_add(1);
            for line in &page.lines {
                if line.cursor != expected || line.truncated {
                    self.gap = true;
                }
                if !line.truncated
                    && let (Some(generation), Some((name, kind))) =
                        (generation, activity_line(&line.text))
                {
                    let observed = line
                        .observed_at
                        .duration_since(UNIX_EPOCH)
                        .ok()
                        .and_then(|d| i64::try_from(d.as_millis()).ok())
                        .unwrap_or(at);
                    events.push(Activity {
                        id: 0,
                        at: observed,
                        generation: Some(generation.to_hex()),
                        name: Some(name.to_owned()),
                        kind: kind.to_owned(),
                    });
                }
                expected = line.cursor.saturating_add(1);
            }
            self.cursor = page.next_cursor;
            if page.lines.len() < MAX_PAGE_LINES {
                if page.latest_cursor > self.cursor {
                    self.gap = true;
                    self.cursor = page.latest_cursor;
                }
                break;
            }
        }
        if runtime.game_snapshot().ok().and_then(|s| {
            s.generation
                .filter(|_| matches!(s.phase, ProcessPhase::Running | ProcessPhase::Stopping))
        }) != generation
        {
            events.clear();
            self.gap = true;
        }
        if self.gap {
            events.push(Activity {
                id: 0,
                at,
                generation: generation.map(|g| g.to_hex()),
                name: None,
                kind: "gap".into(),
            });
            self.gap = false;
        }
        events
    }
    fn sample(&mut self, runtime: &RuntimeHandle, at: i64, now: Instant) -> Sample {
        let snapshot = runtime.game_snapshot().ok();
        let generation = snapshot.and_then(|s| s.generation);
        let identity = runtime
            .metrics_identity()
            .filter(|(g, _, _)| generation == Some(*g));
        let game = identity.and_then(|(_, pid, start)| native::process(pid, Some(start)));
        let mut game_cpu = self
            .game_cpu
            .observe(identity.map_or(0, |(_, pid, _)| pid), game, now);
        let mut game_memory = game.map(|p| p.memory as f64);
        // A native read belongs only to the generation observed on both sides.
        if runtime.game_snapshot().ok().and_then(|s| s.generation) != generation {
            game_cpu = None;
            game_memory = None;
        }
        let agent = native::process(std::process::id(), None);
        let agent_cpu = self.agent_cpu.observe(std::process::id(), agent, now);
        let host = native::host();
        let (received, sent) = self.host_network.observe(super::network::interfaces(), now);
        let host_cpu = match (self.host_cpu.take(), host.cpu) {
            (Some((old_busy, old_total)), Some((busy, total))) => busy
                .checked_sub(old_busy)
                .zip(total.checked_sub(old_total))
                .and_then(|(busy, total)| {
                    (total > 0 && busy <= total).then_some(busy as f64 / total as f64 * 100.0)
                }),
            _ => None,
        };
        self.host_cpu = host.cpu;
        let readiness = runtime
            .readiness()
            .ok()
            .filter(|r| r.generation == generation);
        let players = readiness
            .and_then(|r| r.players)
            .filter(|_| runtime.game_snapshot().ok().and_then(|s| s.generation) == generation);
        let phase = snapshot.map_or("unknown", |s| match s.phase {
            ProcessPhase::Stopped => "stopped",
            ProcessPhase::Running => "running",
            ProcessPhase::Stopping => "stopping",
            ProcessPhase::Draining => "draining",
        });
        let readiness = readiness.map_or("unknown", |r| match r.readiness {
            crate::operations::readiness::Readiness::Stopped => "stopped",
            crate::operations::readiness::Readiness::Starting => "starting",
            crate::operations::readiness::Readiness::Ready => "ready",
            crate::operations::readiness::Readiness::Degraded(_) => "degraded",
        });
        let tick = self
            .tick_latest
            .filter(|_| generation == self.tick_generation)
            .map_or(super::ticks::Observation::default(), |(value, _)| value);
        let mut sample = Sample::new(
            at,
            generation.map(|g| g.to_hex()),
            phase.into(),
            readiness.into(),
            [
                game_cpu,
                game_memory,
                agent_cpu,
                agent.map(|p| p.memory as f64),
                host_cpu,
                host.memory_used.map(|v| v as f64),
                host.memory_total.map(|v| v as f64),
                crate::backups::space::available(&self.game).map(|v| v as f64),
                self.backup
                    .as_ref()
                    .and_then(|path| crate::backups::space::available(path))
                    .map(|v| v as f64),
                players.map(|(online, _)| f64::from(online)),
                players.map(|(_, max)| f64::from(max)),
                received,
                sent,
                tick.tps,
                tick.mspt,
                None,
                None,
            ],
        );
        sample.tick_source =
            (tick.tps.is_some() || tick.mspt.is_some()).then(|| "command".to_owned());
        sample.tick_reason = Some(self.tick_reason.to_owned());
        sample
    }
}
