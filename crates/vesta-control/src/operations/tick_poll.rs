//! Private closed tick-command mailbox. The operation runner retains every native
//! receipt to real acknowledgement, including when the sampler stops waiting.
use super::{
    Operations,
    game::{GameLifecycle, LiveGameView},
};
use crate::metrics::{
    self,
    ticks::{Adapter, Correlation, Observation},
};
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, SyncSender},
    },
    time::Instant,
};
use vesta_game_runtime::{
    console::ConsoleCapture,
    process::{GenerationId, GuardedError, GuardedInput, ProcessPhase, RuntimeError},
    readiness::Readiness,
};

type Outcome = Result<Observation, ()>;
struct Request {
    generation: GenerationId,
    adapter: Adapter,
    revision: i64,
    deadline: Instant,
    reply: Option<SyncSender<Outcome>>,
    cancelled: Arc<AtomicBool>,
}
#[derive(Default)]
struct Mailbox {
    request: Option<Request>,
    busy: bool,
}
pub(super) struct Endpoint {
    mailbox: Arc<Mutex<Mailbox>>,
    stopping: Arc<AtomicBool>,
}
impl Endpoint {
    pub(super) fn submit(
        &self,
        generation: GenerationId,
        adapter: Adapter,
        revision: i64,
    ) -> Option<mpsc::Receiver<Outcome>> {
        if self.stopping.load(Ordering::Acquire) {
            return None;
        }
        let mut mailbox = self.mailbox.lock().ok()?;
        if mailbox.busy {
            return None;
        }
        let (reply, result) = mpsc::sync_channel(1);
        mailbox.request = Some(Request {
            generation,
            adapter,
            revision,
            deadline: Instant::now() + metrics::ticks::WINDOW,
            reply: Some(reply),
            cancelled: Arc::new(AtomicBool::new(false)),
        });
        mailbox.busy = true;
        Some(result)
    }
}
enum Phase {
    Writing {
        receipt: GuardedInput<()>,
        marker: Arc<Mutex<Option<(u64, Instant)>>>,
    },
    Reading(Correlation),
}
struct Pending {
    request: Request,
    phase: Phase,
}
pub(super) struct Driver {
    operations: Operations,
    mailbox: Arc<Mutex<Mailbox>>,
    stopping: Arc<AtomicBool>,
    view: Arc<LiveGameView>,
    console: ConsoleCapture,
    pending: Option<Pending>,
}
fn consent(operations: &Operations, revision: i64) -> bool {
    operations
        .db
        .read(|c| {
            let (stored, document) =
                metrics::effective_policy(c, crate::config::MetricsSettings::default())?;
            Ok(stored == Some(revision)
                && document.settings.enabled
                && document.command_polling.enabled)
        })
        .unwrap_or(false)
}
fn ready(view: &LiveGameView, generation: GenerationId) -> bool {
    view.snapshot()
        .is_ok_and(|s| s.phase == ProcessPhase::Running && s.generation == Some(generation))
        && view
            .readiness()
            .is_ok_and(|r| r.generation == Some(generation) && r.readiness == Readiness::Ready)
}
impl Driver {
    pub(super) fn new(
        operations: Operations,
        stopping: Arc<AtomicBool>,
        view: Arc<LiveGameView>,
        console: ConsoleCapture,
    ) -> (Endpoint, Self) {
        let mailbox = Arc::new(Mutex::new(Mailbox::default()));
        (
            Endpoint {
                mailbox: mailbox.clone(),
                stopping: stopping.clone(),
            },
            Self {
                operations,
                mailbox,
                stopping,
                view,
                console,
                pending: None,
            },
        )
    }
    pub(super) fn writer_pending(&self) -> bool {
        self.pending
            .as_ref()
            .is_some_and(|p| matches!(p.phase, Phase::Writing { .. }))
    }
    fn clear(&self) {
        if let Ok(mut mailbox) = self.mailbox.lock() {
            mailbox.busy = false;
        }
    }
    fn respond(request: &mut Request, result: Outcome) {
        if let Some(reply) = request.reply.take() {
            let _ = reply.try_send(result);
        }
        request.cancelled.store(true, Ordering::Release);
    }
    fn alive(&self, request: &Request) -> bool {
        !self.stopping.load(Ordering::Acquire)
            && Instant::now() < request.deadline
            && !request.cancelled.load(Ordering::Acquire)
            && ready(&self.view, request.generation)
            && consent(&self.operations, request.revision)
    }
    pub(super) fn advance(
        &mut self,
        game: &mut GameLifecycle,
        may_enqueue: bool,
        uncertain: bool,
    ) -> Option<RuntimeError> {
        if self.pending.is_none() && may_enqueue {
            let request = self.mailbox.lock().ok().and_then(|mut m| m.request.take());
            if let Some(mut request) = request {
                if uncertain || !self.alive(&request) {
                    Self::respond(&mut request, Err(()));
                    self.clear();
                } else {
                    let operations = self.operations.clone();
                    let stopping = self.stopping.clone();
                    let view = self.view.clone();
                    let console = self.console.clone();
                    let revision = request.revision;
                    let generation = request.generation;
                    let deadline = request.deadline;
                    let cancelled = request.cancelled.clone();
                    let marker = Arc::new(Mutex::new(None));
                    let mark = marker.clone();
                    let queued = game.process.queue_command_guarded(
                        generation,
                        request.adapter.command(),
                        move || {
                            if stopping.load(Ordering::Acquire)
                                || cancelled.load(Ordering::Acquire)
                                || Instant::now() >= deadline
                                || !ready(&view, generation)
                                || !consent(&operations, revision)
                            {
                                return Err(());
                            }
                            let cursor = console.snapshot(0, 1).map_err(|_| ())?.latest_cursor;
                            *mark.lock().map_err(|_| ())? = Some((cursor, Instant::now()));
                            Ok(())
                        },
                    );
                    match queued {
                        Ok(receipt) => {
                            self.pending = Some(Pending {
                                request,
                                phase: Phase::Writing { receipt, marker },
                            })
                        }
                        Err(error) => {
                            Self::respond(&mut request, Err(()));
                            self.clear();
                            if error == RuntimeError::ObserveFailed {
                                return Some(error);
                            }
                        }
                    }
                }
            }
        }
        let mut pending = self.pending.take()?;
        if uncertain || !self.alive(&pending.request) {
            Self::respond(&mut pending.request, Err(()));
        }
        match pending.phase {
            Phase::Writing {
                mut receipt,
                marker,
            } => match receipt.try_result() {
                None => {
                    self.pending = Some(Pending {
                        request: pending.request,
                        phase: Phase::Writing { receipt, marker },
                    })
                }
                Some(Ok(())) if pending.request.reply.is_some() => {
                    if let Some((cursor, started)) = marker.lock().ok().and_then(|m| *m) {
                        self.pending = Some(Pending {
                            phase: Phase::Reading(Correlation::new(
                                pending.request.adapter,
                                cursor,
                                started,
                            )),
                            request: pending.request,
                        });
                    } else {
                        Self::respond(&mut pending.request, Err(()));
                        self.clear();
                    }
                }
                Some(Err(GuardedError::Runtime(RuntimeError::ObserveFailed))) => {
                    Self::respond(&mut pending.request, Err(()));
                    self.clear();
                    return Some(RuntimeError::ObserveFailed);
                }
                Some(_) => {
                    Self::respond(&mut pending.request, Err(()));
                    self.clear();
                }
            },
            Phase::Reading(mut correlation) => {
                if pending.request.reply.is_none() || correlation.expired(Instant::now()) {
                    Self::respond(&mut pending.request, Err(()));
                    self.clear();
                    return None;
                }
                let Ok(page) = self
                    .console
                    .snapshot(correlation.cursor, metrics::ticks::MAX_LINES)
                else {
                    Self::respond(&mut pending.request, Err(()));
                    self.clear();
                    return None;
                };
                if page.gap {
                    Self::respond(&mut pending.request, Err(()));
                    self.clear();
                    return None;
                }
                for line in page.lines {
                    match correlation.line(line.cursor, line.truncated, &line.text, Instant::now())
                    {
                        Ok(Some(observation)) => {
                            // Check stored consent again immediately before publishing data.
                            let result = if self.alive(&pending.request) {
                                Ok(observation)
                            } else {
                                Err(())
                            };
                            Self::respond(&mut pending.request, result);
                            self.clear();
                            return None;
                        }
                        Ok(None) => {}
                        Err(()) => {
                            Self::respond(&mut pending.request, Err(()));
                            self.clear();
                            return None;
                        }
                    }
                }
                self.pending = Some(Pending {
                    request: pending.request,
                    phase: Phase::Reading(correlation),
                });
            }
        }
        None
    }
}
