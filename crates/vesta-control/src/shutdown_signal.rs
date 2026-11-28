//! Register process shutdown notifications before admitting work. Windows keeps
//! the native end-session callback alive while bounded application cleanup runs.
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

const NORMAL_BUDGET: Duration = Duration::from_secs(60);
const SESSION_BUDGET: Duration = Duration::from_secs(2);
#[cfg(windows)]
const CALLBACK_WAIT: Duration = Duration::from_secs(3);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShutdownReason {
    Interrupt,
    Terminate,
    ConsoleClose,
    SessionEnding,
    Logoff,
    Shutdown,
}
impl ShutdownReason {
    pub fn is_system(self) -> bool {
        !matches!(self, Self::Interrupt)
    }
    fn budget(self) -> Duration {
        match self {
            Self::ConsoleClose | Self::SessionEnding | Self::Logoff | Self::Shutdown => {
                SESSION_BUDGET
            }
            Self::Interrupt | Self::Terminate => NORMAL_BUDGET,
        }
    }
}
#[derive(Clone, Copy, Debug)]
pub struct ShutdownRequest {
    pub reason: ShutdownReason,
    pub deadline: Instant,
}
impl ShutdownRequest {
    pub fn remaining(self) -> Duration {
        self.deadline.saturating_duration_since(Instant::now())
    }
}
struct State {
    request: Mutex<Option<ShutdownRequest>>,
    ready: tokio::sync::Notify,
    complete: Mutex<bool>,
    completed: Condvar,
}
impl State {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            request: Mutex::new(None),
            ready: tokio::sync::Notify::new(),
            complete: Mutex::new(false),
            completed: Condvar::new(),
        })
    }
    fn notify_at(&self, reason: ShutdownReason, now: Instant) {
        let mut request = self.request.lock().unwrap_or_else(|p| p.into_inner());
        let deadline = now + reason.budget();
        match request.as_mut() {
            None => *request = Some(ShutdownRequest { reason, deadline }),
            Some(previous) => {
                // An OS shutdown can supersede Ctrl+C already being drained.
                // Duplicate notifications must never extend the first deadline.
                previous.deadline = previous.deadline.min(deadline);
                if reason.is_system() {
                    previous.reason = reason;
                }
            }
        }
        drop(request);
        self.ready.notify_one();
    }
    fn current(&self) -> Option<ShutdownRequest> {
        *self.request.lock().unwrap_or_else(|p| p.into_inner())
    }
    fn finish(&self) {
        *self.complete.lock().unwrap_or_else(|p| p.into_inner()) = true;
        self.completed.notify_all();
    }
    #[cfg(windows)]
    fn notify_and_wait(&self, reason: ShutdownReason) {
        self.notify_at(reason, Instant::now());
        let done = self.complete.lock().unwrap_or_else(|p| p.into_inner());
        // This native callback owns the OS notification. Never acknowledge it
        // and then rely on the application receiving more execution time.
        drop(
            self.completed
                .wait_timeout_while(done, CALLBACK_WAIT, |done| !*done),
        );
    }
}

#[derive(Clone)]
pub struct ShutdownObserver {
    state: Arc<State>,
}
impl ShutdownObserver {
    pub fn current_request(&self) -> Option<ShutdownRequest> {
        self.state.current()
    }
}

pub struct ShutdownSignal {
    state: Arc<State>,
    #[cfg(unix)]
    _watcher: UnixRegistration,
    #[cfg(windows)]
    _native: windows::Registration,
}
impl ShutdownSignal {
    /// Register before listeners or jobs start. Native delivery is independent
    /// of the application runtime, so blocked startup cannot hide OS shutdown.
    pub fn register() -> Result<Self, &'static str> {
        let state = State::new();
        #[cfg(unix)]
        {
            let watcher = UnixRegistration::open(state.clone())?;
            Ok(Self {
                state,
                _watcher: watcher,
            })
        }
        #[cfg(windows)]
        {
            let native = windows::Registration::open(state.clone())?;
            Ok(Self {
                state,
                _native: native,
            })
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = state;
            Err("shutdown_signal_unsupported")
        }
    }
    pub async fn wait(&mut self) -> Result<ShutdownRequest, &'static str> {
        if let Some(request) = self.state.current() {
            return Ok(request);
        }
        while self.state.current().is_none() {
            self.state.ready.notified().await;
        }
        self.state.current().ok_or("shutdown_signal_closed")
    }
    /// Poll during cleanup: later system shutdown can shorten an earlier Ctrl+C
    /// deadline. The independent platform watcher updates this during cleanup.
    pub fn current_request(&self) -> Option<ShutdownRequest> {
        self.state.current()
    }
    /// The watchdog can observe later OS requests independently of main cleanup.
    pub fn observer(&self) -> ShutdownObserver {
        ShutdownObserver {
            state: self.state.clone(),
        }
    }
    /// Call after actual cleanup (or once unclean-exit state has been persisted).
    pub fn complete(&self) {
        self.state.finish();
    }
}
impl Drop for ShutdownSignal {
    fn drop(&mut self) {
        self.state.finish();
    }
}

/// Synchronous setup wrappers use the same independently driven monitor.
pub type ShutdownMonitor = ShutdownSignal;

#[cfg(unix)]
struct UnixRegistration {
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}
#[cfg(unix)]
impl UnixRegistration {
    fn open(state: Arc<State>) -> Result<Self, &'static str> {
        let (ready, registered) = std::sync::mpsc::sync_channel(1);
        let (stop, mut stopped) = tokio::sync::oneshot::channel();
        let thread=std::thread::Builder::new().name("vesta-process-signals".into()).spawn(move || {
            let runtime=match tokio::runtime::Builder::new_current_thread().enable_all().build() {
                Ok(runtime)=>runtime,Err(_)=>{let _=ready.send(Err("shutdown_signal_runtime_failed"));return;}
            };
            runtime.block_on(async move {
                let streams=(tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt()),tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()));
                let (mut interrupt,mut terminate)=match streams {
                    (Ok(interrupt),Ok(terminate))=>(interrupt,terminate),
                    _=>{let _=ready.send(Err("shutdown_signal_registration_failed"));return;}
                };
                if ready.send(Ok(())).is_err(){return;}
                loop {
                    let reason=tokio::select! {
                        signal=interrupt.recv()=>{if signal.is_none(){break;}ShutdownReason::Interrupt},
                        signal=terminate.recv()=>{if signal.is_none(){break;}ShutdownReason::Terminate},
                        _=&mut stopped=>break,
                    };
                    state.notify_at(reason,Instant::now());
                }
            });
        }).map_err(|_|"shutdown_signal_registration_failed")?;
        match registered.recv() {
            Ok(Ok(())) => Ok(Self {
                stop: Some(stop),
                thread: Some(thread),
            }),
            result => {
                let _ = stop.send(());
                let _ = thread.join();
                Err(result
                    .ok()
                    .and_then(Result::err)
                    .unwrap_or("shutdown_signal_registration_failed"))
            }
        }
    }
}
#[cfg(unix)]
impl Drop for UnixRegistration {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        // This thread runs only OS signals and its stop channel, never app work.
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[cfg(windows)]
#[path = "shutdown_signal_windows.rs"]
mod windows;

#[cfg(windows)]
pub(crate) fn notify_owned_child(pid: u32, reason: ShutdownReason) -> bool {
    windows::notify_owned_child(pid, reason)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn duplicate_requests_never_extend_deadline_and_os_shutdown_escalates() {
        let state = State::new();
        let start = Instant::now();
        state.notify_at(ShutdownReason::Interrupt, start);
        assert_eq!(state.current().unwrap().deadline, start + NORMAL_BUDGET);
        state.notify_at(ShutdownReason::Interrupt, start + Duration::from_secs(1));
        assert_eq!(state.current().unwrap().deadline, start + NORMAL_BUDGET);
        state.notify_at(
            ShutdownReason::SessionEnding,
            start + Duration::from_secs(2),
        );
        let request = state.current().unwrap();
        assert!(request.reason.is_system());
        assert_eq!(
            request.deadline,
            start + Duration::from_secs(2) + SESSION_BUDGET
        );
        state.notify_at(ShutdownReason::Interrupt, start + Duration::from_secs(3));
        assert!(state.current().unwrap().reason.is_system());
        assert_eq!(state.current().unwrap().deadline, request.deadline);
    }
    #[test]
    fn completion_is_idempotent_and_expired_deadline_has_no_budget() {
        let state = State::new();
        state.finish();
        state.finish();
        assert!(*state.complete.lock().unwrap());
        let request = ShutdownRequest {
            reason: ShutdownReason::Terminate,
            deadline: Instant::now() - Duration::from_millis(1),
        };
        assert_eq!(request.remaining(), Duration::ZERO);
    }
    #[cfg(unix)]
    #[test]
    fn registration_captures_both_unix_signals_before_waiting() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let mut signal = ShutdownSignal::register().unwrap();
            assert!(signal.current_request().is_none());
            signal
                .state
                .notify_at(ShutdownReason::Terminate, Instant::now());
            assert_eq!(
                signal.wait().await.unwrap().reason,
                ShutdownReason::Terminate
            );
            signal.complete();
        });
    }
    #[cfg(unix)]
    #[test]
    fn os_signal_child() {
        let Some(ready) = std::env::var_os("VESTA_SIGNAL_TEST_READY") else {
            return;
        };
        let expected = match std::env::var("VESTA_SIGNAL_TEST_KIND").unwrap().as_str() {
            "TERM" => ShutdownReason::Terminate,
            "INT" => ShutdownReason::Interrupt,
            _ => panic!("unknown test signal"),
        };
        // No application Tokio runtime: delivery must be independently driven.
        let signal = ShutdownSignal::register().unwrap();
        std::fs::write(ready, b"ready").unwrap();
        let until = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(request) = signal.current_request() {
                assert_eq!(request.reason, expected);
                signal.complete();
                return;
            }
            assert!(Instant::now() < until, "OS signal was not observed");
            std::thread::park_timeout(Duration::from_millis(10));
        }
    }
    #[cfg(unix)]
    #[test]
    fn real_unix_signals_are_received_while_application_thread_is_blocked() {
        for kind in ["TERM", "INT"] {
            let temp = tempfile::tempdir().unwrap();
            let ready = temp.path().join("ready");
            let mut child = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "shutdown_signal::tests::os_signal_child",
                    "--nocapture",
                ])
                .env("VESTA_SIGNAL_TEST_READY", &ready)
                .env("VESTA_SIGNAL_TEST_KIND", kind)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .unwrap();
            let until = Instant::now() + Duration::from_secs(5);
            while !ready.exists() {
                if child.try_wait().unwrap().is_some() {
                    panic!("signal fixture exited before registration");
                }
                if Instant::now() >= until {
                    let _ = child.kill();
                    let _ = child.wait();
                    panic!("signal fixture failed to register");
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            // Only this exact live child PID is signaled, never a process group.
            let result = std::process::Command::new("kill")
                .arg(format!("-{kind}"))
                .arg(child.id().to_string())
                .status()
                .unwrap();
            assert!(result.success());
            let until = Instant::now() + Duration::from_secs(5);
            loop {
                if child.try_wait().unwrap().is_some() {
                    break;
                }
                if Instant::now() >= until {
                    let _ = child.kill();
                    let _ = child.wait();
                    panic!("signal fixture did not exit");
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            let output = child.wait_with_output().unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }
    #[cfg(unix)]
    #[test]
    fn setup_wrapper_forwards_shutdown_to_its_exact_owned_child() {
        for (kind, reason) in [
            ("TERM", ShutdownReason::Terminate),
            ("INT", ShutdownReason::Interrupt),
        ] {
            let temp = tempfile::tempdir().unwrap();
            let ready = temp.path().join("ready");
            let mut child = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "shutdown_signal::tests::os_signal_child",
                    "--nocapture",
                ])
                .env("VESTA_SIGNAL_TEST_READY", &ready)
                .env("VESTA_SIGNAL_TEST_KIND", kind)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .unwrap();
            let until = Instant::now() + Duration::from_secs(5);
            while !ready.exists() {
                if child.try_wait().unwrap().is_some() {
                    panic!("wrapper child exited before registering");
                }
                if Instant::now() >= until {
                    let _ = child.kill();
                    let _ = child.wait();
                    panic!("wrapper fixture failed to register");
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            let state = State::new();
            state.notify_at(reason, Instant::now());
            let observer = ShutdownObserver { state };
            let status = crate::process_shutdown::wait_owned_child(&mut child, &observer);
            let output = child.wait_with_output().unwrap();
            assert!(
                status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }
}
