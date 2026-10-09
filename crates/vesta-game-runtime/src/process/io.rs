use super::{GuardedError, GuardedInput, RuntimeError};
use crate::console::{ConsoleCapture, ConsoleStream};
use std::{
    io::{self, Read, Write},
    process::{Child, ChildStderr, ChildStdin, ChildStdout},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Sender, SyncSender, TrySendError},
    },
    thread::{self, JoinHandle},
};

const QUEUE_LINES: usize = 8;

// Guarded requests use the same bounded, exclusively owned writer as ordinary
// lines. The request retains its callback/core through the actual native write.
enum Input {
    Line(String),
    Guarded(Box<dyn FnOnce(&mut ChildStdin) -> bool + Send>),
}

#[derive(Default)]
struct Failures {
    output: AtomicBool,
    stdin: AtomicBool,
}

struct Pipes {
    stdin: Sender<ChildStdin>,
    stdout: Sender<ChildStdout>,
    stderr: Sender<ChildStderr>,
}

pub(super) struct IoThreads {
    pipes: Option<Pipes>,
    lines: Option<SyncSender<Input>>,
    // Index zero is stdin; the remaining two are output drain workers.
    workers: [Option<JoinHandle<()>>; 3],
    failures: Arc<Failures>,
}
impl IoThreads {
    pub(super) fn prepare(console: &ConsoleCapture) -> Result<Self, RuntimeError> {
        Self::prepare_with(console, |name, work| {
            thread::Builder::new()
                .name(name.into())
                .spawn(work)
                .map_err(|_| RuntimeError::ThreadFailed)
        })
    }

    fn prepare_with(
        console: &ConsoleCapture,
        mut spawn: impl FnMut(
            &'static str,
            Box<dyn FnOnce() + Send>,
        ) -> Result<JoinHandle<()>, RuntimeError>,
    ) -> Result<Self, RuntimeError> {
        let (stdin_tx, stdin_rx) = mpsc::channel::<ChildStdin>();
        let (stdout_tx, stdout_rx) = mpsc::channel::<ChildStdout>();
        let (stderr_tx, stderr_rx) = mpsc::channel::<ChildStderr>();
        let (lines_tx, lines_rx) = mpsc::sync_channel::<Input>(QUEUE_LINES);
        let mut owned = Self {
            pipes: Some(Pipes {
                stdin: stdin_tx,
                stdout: stdout_tx,
                stderr: stderr_tx,
            }),
            lines: Some(lines_tx),
            workers: std::array::from_fn(|_| None),
            failures: Arc::new(Failures::default()),
        };
        let failures = owned.failures.clone();
        owned.workers[0] = Some(spawn(
            "game-stdin",
            Box::new(move || {
                if let Ok(mut pipe) = stdin_rx.recv() {
                    for input in lines_rx {
                        let wrote = match input {
                            Input::Line(line) => pipe.write_all(line.as_bytes()).is_ok(),
                            Input::Guarded(effect) => effect(&mut pipe),
                        };
                        if !wrote {
                            failures.stdin.store(true, Ordering::Release);
                            break;
                        }
                    }
                }
            }),
        )?);
        let capture = console.clone();
        let failures = owned.failures.clone();
        owned.workers[1] = Some(spawn(
            "game-stdout",
            Box::new(move || {
                if let Ok(pipe) = stdout_rx.recv() {
                    drain(capture, ConsoleStream::Stdout, pipe, &failures);
                }
            }),
        )?);
        let capture = console.clone();
        let failures = owned.failures.clone();
        owned.workers[2] = Some(spawn(
            "game-stderr",
            Box::new(move || {
                if let Ok(pipe) = stderr_rx.recv() {
                    drain(capture, ConsoleStream::Stderr, pipe, &failures);
                }
            }),
        )?);
        Ok(owned)
    }

    pub(super) fn attach(&mut self, child: &mut Child) -> Result<(), RuntimeError> {
        let result = self.attach_pipes(child);
        if result.is_err() {
            // Partial attachment is not successful capture, even if some workers return.
            self.failures.output.store(true, Ordering::Release);
        }
        result
    }

    fn attach_pipes(&mut self, child: &mut Child) -> Result<(), RuntimeError> {
        let pipes = self.pipes.take().ok_or(RuntimeError::StartupUncertain)?;
        let Some(stdin) = child.stdin.take() else {
            self.failures.stdin.store(true, Ordering::Release);
            return Err(RuntimeError::StartupUncertain);
        };
        if let Err(failure) = pipes.stdin.send(stdin) {
            child.stdin = Some(failure.0);
            self.failures.stdin.store(true, Ordering::Release);
            return Err(RuntimeError::StartupUncertain);
        }
        // Unattempted and unsuccessfully dispatched pipes stay with the exact child.
        let stdout = child.stdout.take().ok_or(RuntimeError::StartupUncertain)?;
        if let Err(failure) = pipes.stdout.send(stdout) {
            child.stdout = Some(failure.0);
            return Err(RuntimeError::StartupUncertain);
        }
        let stderr = child.stderr.take().ok_or(RuntimeError::StartupUncertain)?;
        if let Err(failure) = pipes.stderr.send(stderr) {
            child.stderr = Some(failure.0);
            return Err(RuntimeError::StartupUncertain);
        }
        Ok(())
    }

    #[cfg(test)]
    pub(super) fn gated_stdin_for_test(
        console: &ConsoleCapture,
        entered: Sender<()>,
        release: mpsc::Receiver<()>,
    ) -> Result<Self, RuntimeError> {
        let mut gate = Some((entered, release));
        Self::prepare_with(console, |name, work| {
            let gate = if name == "game-stdin" {
                gate.take()
            } else {
                None
            };
            thread::Builder::new()
                .name(name.into())
                .spawn(move || {
                    if let Some((entered, release)) = gate {
                        entered.send(()).unwrap();
                        // Disconnection allows panic cleanup; timeout never counts as evidence.
                        match release.recv_timeout(std::time::Duration::from_secs(15)) {
                            Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => (),
                            Err(mpsc::RecvTimeoutError::Timeout) => {
                                panic!("stdin fixture gate timed out")
                            }
                        }
                    }
                    work();
                })
                .map_err(|_| RuntimeError::ThreadFailed)
        })
    }

    #[cfg(test)]
    pub(super) fn disconnect_stdout_for_test(&mut self) {
        let (sender, receiver) = mpsc::channel();
        drop(receiver);
        self.pipes.as_mut().unwrap().stdout = sender;
    }

    pub(super) fn queue(&self, line: &str) -> Result<(), RuntimeError> {
        let sender = self.lines.as_ref().ok_or(RuntimeError::StdinUnavailable)?;
        let mut payload = String::with_capacity(line.len() + 1);
        payload.push_str(line);
        payload.push('\n');
        sender
            .try_send(Input::Line(payload))
            .map_err(|failure| match failure {
                TrySendError::Full(_) => RuntimeError::StdinBusy,
                TrySendError::Disconnected(_) => RuntimeError::StdinUnavailable,
            })
    }

    pub(super) fn guarded_stop<E, F>(&self, mut guard: F) -> Result<GuardedInput<E>, RuntimeError>
    where
        E: Send + 'static,
        F: FnMut() -> Result<(), E> + Send + 'static,
    {
        // All request preparation and queue failures precede authorization. Queue
        // admission is never reported as a native write or an effect marker.
        let sender = self.lines.as_ref().ok_or(RuntimeError::StdinUnavailable)?;
        let (ack, receiver) = mpsc::channel();
        let work = Input::Guarded(Box::new(move |pipe| {
            let result = match guard() {
                Err(error) => Err(GuardedError::Guard(error)),
                // The fixed native call directly follows the fresh guard. No
                // cached permit, allocation, dispatch or external callback here.
                Ok(()) => pipe
                    .write_all(b"stop\n")
                    .map_err(|_| GuardedError::Runtime(RuntimeError::StdinUnavailable)),
            };
            let native_failed = matches!(&result, Err(GuardedError::Runtime(_)));
            // The callback/core stays owned through native write_all. Dispose
            // it before acknowledging: an observed ACK proves this callback can
            // no longer execute. The concrete runtime still retains its owner,
            // claim and this exact native writer handle independently.
            drop(guard);
            let _ = ack.send(result);
            !native_failed
        }));
        sender.try_send(work).map_err(|failure| match failure {
            TrySendError::Full(_) => RuntimeError::StdinBusy,
            TrySendError::Disconnected(_) => RuntimeError::StdinUnavailable,
        })?;
        Ok(GuardedInput {
            receiver,
            completed: false,
        })
    }

    pub(super) fn close_input(&mut self) {
        self.lines.take();
    }

    pub(super) fn reap_finished(&mut self) {
        for (index, worker) in self.workers.iter_mut().enumerate() {
            if worker.as_ref().is_some_and(JoinHandle::is_finished) {
                if let Some(handle) = worker.take() {
                    if handle.join().is_err() {
                        let flag = if index == 0 {
                            &self.failures.stdin
                        } else {
                            &self.failures.output
                        };
                        flag.store(true, Ordering::Release);
                    }
                }
            }
        }
    }

    pub(super) fn finished(&self) -> bool {
        self.workers.iter().all(Option::is_none)
    }
    #[cfg(test)]
    pub(super) fn retained_workers_for_test(&self) -> usize {
        self.workers
            .iter()
            .filter(|worker| worker.is_some())
            .count()
    }
    pub(super) fn output_failed(&self) -> bool {
        self.failures.output.load(Ordering::Acquire)
    }
    pub(super) fn stdin_failed(&self) -> bool {
        self.failures.stdin.load(Ordering::Acquire)
    }
}
impl Drop for IoThreads {
    fn drop(&mut self) {
        self.pipes.take();
        self.close_input();
        for worker in &mut self.workers {
            if let Some(handle) = worker.take() {
                let _ = handle.join();
            }
        }
    }
}

fn drain(capture: ConsoleCapture, stream: ConsoleStream, mut pipe: impl Read, failures: &Failures) {
    if capture.capture_reader(stream, &mut pipe).is_err() {
        failures.output.store(true, Ordering::Release);
        // A poisoned/exhausted console cannot be allowed to leave a live pipe full.
        // Native read failure is still flagged; no success/complete capture is fabricated.
        let _ = io::copy(&mut pipe, &mut io::sink());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    #[test]
    fn second_and_third_thread_failure_join_every_prepared_worker_before_return() {
        for refuse in [2, 3] {
            let returned = Arc::new(AtomicUsize::new(0));
            let mut calls = 0;
            let result = IoThreads::prepare_with(&ConsoleCapture::new(), |_, work| {
                calls += 1;
                if calls == refuse {
                    return Err(RuntimeError::ThreadFailed);
                }
                let returned = returned.clone();
                Ok(thread::spawn(move || {
                    work();
                    returned.fetch_add(1, Ordering::Release);
                }))
            });
            assert!(matches!(result, Err(RuntimeError::ThreadFailed)));
            assert_eq!(calls, refuse);
            assert_eq!(returned.load(Ordering::Acquire), refuse - 1);
        }
    }

    #[test]
    fn storage_failure_discards_remaining_reader_bytes_and_records_fault() {
        let capture = ConsoleCapture::new();
        capture.poison_for_test();
        let failures = Failures::default();
        let mut bytes = b"trigger\n".to_vec();
        bytes.extend_from_slice(&vec![b'x'; 1024 * 1024]);
        let expected = bytes.len() as u64;
        let mut reader = io::Cursor::new(bytes);
        drain(capture, ConsoleStream::Stdout, &mut reader, &failures);
        assert_eq!(reader.position(), expected);
        assert!(failures.output.load(Ordering::Acquire));
    }
}
