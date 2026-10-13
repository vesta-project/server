//! Runner-side readiness: feeds the owned console and generation changes into the
//! pure tracker and runs status probes on a bounded worker, one at a time.
//!
//! Works identically for an agent-owned child and a supervised game, because both
//! publish into the lifecycle's console capture. Results from an older generation
//! are discarded. Readiness is observation only; it never starts or stops anything.
use std::{
    net::SocketAddr,
    sync::mpsc,
    thread::JoinHandle,
    time::{Duration, Instant},
};
pub use vesta_game_runtime::readiness::{DegradedReason, Readiness};
use vesta_game_runtime::{
    console::{ConsoleCapture, MAX_PAGE_LINES},
    process::GenerationId,
    readiness::{ProbeError, ReadinessConfig, ReadinessTracker, StatusSummary},
};

const TRACK_INTERVAL: Duration = Duration::from_millis(250);
const MAX_PAGES_PER_TICK: usize = 8;

type ProbeResult = (GenerationId, Result<StatusSummary, ProbeError>);

/// Published, bounded readiness facts.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReadinessView {
    pub readiness: Readiness,
    /// False when no status probe address is known: readiness is log-only.
    pub probed: bool,
    /// Most recent successful status ping (at most 30 seconds old), never `list`.
    pub players: Option<(u32, u32)>,
    pub generation: Option<GenerationId>,
}

pub(super) struct ReadinessWatch {
    tracker: ReadinessTracker,
    console: ConsoleCapture,
    cursor: u64,
    generation: Option<GenerationId>,
    probe: Option<(GenerationId, mpsc::Receiver<ProbeResult>, JoinHandle<()>)>,
    tracked: Option<Instant>,
    timeout: Duration,
    players: Option<(Instant, StatusSummary)>,
}
impl ReadinessWatch {
    pub(super) fn new(config: ReadinessConfig, console: ConsoleCapture) -> Self {
        let timeout = config.probe_timeout;
        Self {
            tracker: ReadinessTracker::new(config),
            console,
            cursor: 0,
            generation: None,
            probe: None,
            tracked: None,
            timeout,
            players: None,
        }
    }

    /// Advance with the current live generation and the console cursor after which
    /// its output begins (`None` when that boundary is unknown). Cheap when called often.
    pub(super) fn track(
        &mut self,
        generation: Option<GenerationId>,
        floor: Option<u64>,
    ) -> ReadinessView {
        let now = Instant::now();
        if self.generation != generation {
            self.generation = generation;
            self.players = None;
            match generation {
                Some(_) => {
                    self.tracker.generation_started(now);
                    match floor {
                        // Unread lines up to the boundary belong to an earlier generation.
                        Some(floor) => self.cursor = self.cursor.max(floor),
                        None => {
                            // Unknown boundary: skip the backlog rather than misattribute
                            // it, and say output may have been missed.
                            if let Ok(page) = self.console.snapshot(0, 1) {
                                self.cursor = page.latest_cursor;
                            }
                            self.tracker.observe_gap();
                        }
                    }
                }
                None => self.tracker.generation_stopped(),
            }
            self.tracked = None;
        }
        if self
            .tracked
            .is_none_or(|at| now.duration_since(at) >= TRACK_INTERVAL)
        {
            self.tracked = Some(now);
            self.scan();
            self.collect_probe(now);
            self.start_probe(now);
        }
        ReadinessView {
            readiness: self.tracker.state(now),
            probed: self.tracker.probe_configured(),
            generation: self.generation,
            players: self
                .players
                .filter(|(at, _)| now.duration_since(*at) <= Duration::from_secs(30))
                .map(|(_, s)| (s.online, s.max)),
        }
    }

    fn scan(&mut self) {
        for _ in 0..MAX_PAGES_PER_TICK {
            let Ok(page) = self.console.snapshot(self.cursor, MAX_PAGE_LINES) else {
                self.tracker.observe_gap();
                return;
            };
            // Any skipped cursor (evicted prefix or a mirrored upstream gap) is lost output.
            let mut expected = self.cursor + 1;
            for line in &page.lines {
                if line.cursor != expected {
                    self.tracker.observe_gap();
                }
                if self.generation.is_some() {
                    self.tracker.observe_line(&line.text);
                }
                expected = line.cursor + 1;
            }
            self.cursor = page.next_cursor;
            if page.lines.len() < MAX_PAGE_LINES {
                // A trailing gap consumes cursors with no line after it.
                if page.latest_cursor > self.cursor {
                    self.tracker.observe_gap();
                    self.cursor = page.latest_cursor;
                }
                return;
            }
        }
    }

    fn collect_probe(&mut self, now: Instant) {
        let Some((generation, receiver, _)) = &self.probe else {
            return;
        };
        let result = match receiver.try_recv() {
            Ok(result) => Some(result),
            Err(mpsc::TryRecvError::Empty) => return,
            Err(mpsc::TryRecvError::Disconnected) => None,
        };
        let generation = *generation;
        if let Some((_, _, worker)) = self.probe.take() {
            let _ = worker.join();
        }
        // Discard results for a generation that is no longer current.
        if self.generation == Some(generation) {
            let result = result.map_or(Err(ProbeError::Connect), |(_, result)| result);
            self.players = result.ok().map(|s| (now, s));
            self.tracker.record_probe(now, result);
        }
    }

    fn start_probe(&mut self, now: Instant) {
        let (None, Some(generation)) = (&self.probe, self.generation) else {
            return;
        };
        let Some(address) = self.tracker.probe_due(now) else {
            return;
        };
        let (sender, receiver) = mpsc::sync_channel(1);
        let timeout = self.timeout;
        let worker = std::thread::Builder::new()
            .name("vesta-readiness-probe".into())
            .spawn(move || {
                let result = vesta_game_runtime::readiness::status_probe(address, timeout);
                let _ = sender.send((generation, result));
            });
        match worker {
            Ok(worker) => self.probe = Some((generation, receiver, worker)),
            Err(_) => self.tracker.record_probe(now, Err(ProbeError::Connect)),
        }
    }
}
impl Drop for ReadinessWatch {
    fn drop(&mut self) {
        // Bounded by the probe timeout; never detached.
        if let Some((_, _, worker)) = self.probe.take() {
            let _ = worker.join();
        }
    }
}

/// Probe address from the game's `server.properties`, if readable.
pub fn probe_from_properties(text: &str) -> Option<SocketAddr> {
    let mut address = crate::config::game_bind_address(text.as_bytes())?;
    if address.ip().is_unspecified() {
        address.set_ip(if address.is_ipv4() {
            std::net::Ipv4Addr::LOCALHOST.into()
        } else {
            std::net::Ipv6Addr::LOCALHOST.into()
        });
    }
    Some(address)
}

#[cfg(test)]
mod tests {
    use super::*;
    use vesta_game_runtime::console::ConsoleStream;

    const DONE: &str = "[12:00:00] [Server thread/INFO]: Done (1.234s)! For help, type \"help\"";

    fn generation(byte: u8) -> GenerationId {
        GenerationId::parse_hex(&format!("{byte:02x}").repeat(16)).unwrap()
    }

    fn settle(watch: &mut ReadinessWatch, generation: Option<GenerationId>) -> ReadinessView {
        let floor = watch.console.snapshot(0, 1).unwrap().latest_cursor;
        watch.tracked = None;
        // Each test generation begins after everything already captured.
        let floor = if watch.generation == generation {
            None
        } else {
            Some(floor)
        };
        watch.track(generation, floor)
    }

    #[test]
    fn log_only_readiness_follows_console_and_generations() {
        let console = ConsoleCapture::new();
        let mut watch = ReadinessWatch::new(ReadinessConfig::default(), console.clone());
        assert_eq!(settle(&mut watch, None).readiness, Readiness::Stopped);
        // A done line from before this generation must not count for it.
        console.ingest(ConsoleStream::Stdout, DONE, false).unwrap();
        settle(&mut watch, None);
        let first = generation(1);
        let view = settle(&mut watch, Some(first));
        assert_eq!(view.readiness, Readiness::Starting);
        assert!(!view.probed);
        // A chat line imitating the done text is not readiness evidence.
        console
            .ingest(
                ConsoleStream::Stdout,
                "[12:00:01] [Server thread/INFO]: <Steve> Done (1s)! For help, type \"help\"",
                false,
            )
            .unwrap();
        assert_eq!(
            settle(&mut watch, Some(first)).readiness,
            Readiness::Starting
        );
        console.ingest(ConsoleStream::Stdout, DONE, false).unwrap();
        assert_eq!(settle(&mut watch, Some(first)).readiness, Readiness::Ready);
        // A new generation starts over.
        assert_eq!(
            settle(&mut watch, Some(generation(2))).readiness,
            Readiness::Starting
        );
        assert_eq!(settle(&mut watch, None).readiness, Readiness::Stopped);
    }

    #[test]
    fn configured_probe_is_required_and_a_failing_probe_is_not_ready() {
        // Reserve then release a loopback port so connections are refused.
        let address = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap();
        let console = ConsoleCapture::new();
        let mut watch = ReadinessWatch::new(
            ReadinessConfig {
                probe: Some(address),
                probe_interval: Duration::from_millis(1),
                probe_timeout: Duration::from_millis(500),
                ..ReadinessConfig::default()
            },
            console.clone(),
        );
        let first = generation(3);
        settle(&mut watch, Some(first));
        console.ingest(ConsoleStream::Stdout, DONE, false).unwrap();
        let until = Instant::now() + Duration::from_secs(10);
        let mut view = settle(&mut watch, Some(first));
        while watch.probe.is_some() || view.readiness == Readiness::Starting {
            assert!(Instant::now() < until, "probe completes");
            std::thread::sleep(Duration::from_millis(20));
            view = settle(&mut watch, Some(first));
            if watch.probe.is_none() {
                break;
            }
        }
        assert!(view.probed);
        assert_ne!(view.readiness, Readiness::Ready);
    }

    #[test]
    fn gaps_inside_a_page_and_unknown_boundaries_are_lost_output() {
        let console = ConsoleCapture::new();
        let mut watch = ReadinessWatch::new(ReadinessConfig::default(), console.clone());
        let first = generation(4);
        settle(&mut watch, Some(first));
        console
            .ingest(ConsoleStream::Stdout, "starting", false)
            .unwrap();
        console.mark_gap().unwrap();
        console
            .ingest(ConsoleStream::Stdout, "more", false)
            .unwrap();
        assert_eq!(
            settle(&mut watch, Some(first)).readiness,
            Readiness::Degraded(vesta_game_runtime::readiness::DegradedReason::ConsoleGap)
        );
        // Without a known boundary, earlier output is skipped and reported as a gap.
        console.ingest(ConsoleStream::Stdout, DONE, false).unwrap();
        watch.tracked = None;
        let view = watch.track(Some(generation(5)), None);
        assert_ne!(view.readiness, Readiness::Ready);
    }

    #[test]
    fn metrics_ping_counts_are_fresh_and_generation_bound() {
        let console = ConsoleCapture::new();
        let mut watch = ReadinessWatch::new(ReadinessConfig::default(), console);
        let first = generation(6);
        settle(&mut watch, Some(first));
        let (sender, receiver) = mpsc::sync_channel(1);
        sender
            .send((
                first,
                Ok(StatusSummary {
                    online: 2,
                    max: 20,
                    protocol: 47,
                }),
            ))
            .unwrap();
        watch.probe = Some((first, receiver, std::thread::spawn(|| {})));
        watch.collect_probe(Instant::now());
        assert_eq!(settle(&mut watch, Some(first)).players, Some((2, 20)));
        watch.players = Some((
            Instant::now() - Duration::from_secs(31),
            StatusSummary {
                online: 2,
                max: 20,
                protocol: 47,
            },
        ));
        assert!(settle(&mut watch, Some(first)).players.is_none());
        let (sender, receiver) = mpsc::sync_channel(1);
        sender.send((first, Err(ProbeError::Connect))).unwrap();
        watch.probe = Some((first, receiver, std::thread::spawn(|| {})));
        watch.collect_probe(Instant::now());
        assert!(settle(&mut watch, Some(first)).players.is_none());
        watch.players = Some((
            Instant::now(),
            StatusSummary {
                online: 2,
                max: 20,
                protocol: 47,
            },
        ));
        assert!(settle(&mut watch, Some(generation(7))).players.is_none());
    }
}
