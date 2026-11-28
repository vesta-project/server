//! Process-wide deadline enforcement for confirmed shutdown. Owners remain held
//! by their coordinators until native work actually finishes. At the OS boundary
//! an expired deadline exits uncleanly instead of entering an unbounded Drop join.
use crate::shutdown_signal::{ShutdownObserver, ShutdownReason, ShutdownRequest};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

struct State {
    done: bool,
    request: Option<ShutdownRequest>,
}
pub struct ExitWatchdog {
    state: Arc<(Mutex<State>, Condvar)>,
    observer: ShutdownObserver,
    thread: Option<JoinHandle<()>>,
}
impl ExitWatchdog {
    /// Arm before opening databases/listeners: OS notifications must still have a
    /// bounded outcome if startup, a native callback or a writer stops responding.
    pub fn arm(observer: ShutdownObserver) -> Result<Self, &'static str> {
        let state = Arc::new((
            Mutex::new(State {
                done: false,
                request: None,
            }),
            Condvar::new(),
        ));
        let watched = state.clone();
        let signals = observer.clone();
        let thread = std::thread::Builder::new()
            .name("vesta-shutdown-deadline".into())
            .spawn(move || {
                let (lock, wake) = &*watched;
                loop {
                    let guard = lock.lock().unwrap_or_else(|p| p.into_inner());
                    if guard.done {
                        return;
                    }
                    let deadline = earliest(guard.request, signals.current_request());
                    let wait = if let Some(request) = deadline {
                        let remaining = request.remaining();
                        if remaining.is_zero() {
                            // Do not run ownership destructors here: they deliberately
                            // retain live native work. Never publish a clean marker.
                            std::process::exit(1);
                        }
                        remaining.min(Duration::from_millis(25))
                    } else {
                        Duration::from_millis(100)
                    };
                    let _ = wake
                        .wait_timeout(guard, wait)
                        .unwrap_or_else(|p| p.into_inner());
                }
            })
            .map_err(|_| "shutdown_watchdog_unavailable")?;
        Ok(Self {
            state,
            observer,
            thread: Some(thread),
        })
    }
    pub fn begin(&self, request: ShutdownRequest) {
        let (lock, wake) = &*self.state;
        let mut state = lock.lock().unwrap_or_else(|p| p.into_inner());
        state.request = earliest(state.request, Some(request));
        wake.notify_all();
        drop(state);
        // Any diagnostic IO happens on the caller, after arming the independent
        // deadline. A blocked stderr pipe must never block deadline enforcement.
        eprintln!(
            "shutdown_started: {:?}; unfinished work may require recovery",
            request.reason
        );
    }
    pub fn begin_local(&self) {
        self.begin(ShutdownRequest {
            reason: ShutdownReason::Interrupt,
            deadline: Instant::now() + Duration::from_secs(60),
        });
    }
    pub fn remaining(&self) -> Duration {
        let state = self.state.0.lock().unwrap_or_else(|p| p.into_inner());
        earliest(state.request, self.observer.current_request())
            .map_or(Duration::from_secs(60), ShutdownRequest::remaining)
    }
    pub fn finish(mut self) {
        self.complete();
    }
    fn complete(&mut self) {
        let (lock, wake) = &*self.state;
        lock.lock().unwrap_or_else(|p| p.into_inner()).done = true;
        wake.notify_all();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
impl Drop for ExitWatchdog {
    fn drop(&mut self) {
        self.complete();
    }
}
fn earliest(a: Option<ShutdownRequest>, b: Option<ShutdownRequest>) -> Option<ShutdownRequest> {
    match (a, b) {
        (Some(a), Some(b)) => Some(if a.deadline <= b.deadline { a } else { b }),
        (a, b) => a.or(b),
    }
}

/// Wait for the exact child we spawned. A confirmed wrapper shutdown is forwarded
/// once per reason, and completion is reported only after try_wait reaps it.
/// The caller must keep ExitWatchdog alive throughout this wait.
pub fn wait_owned_child(
    child: &mut std::process::Child,
    observer: &ShutdownObserver,
) -> std::process::ExitStatus {
    let mut forwarded = None;
    let mut reported = false;
    loop {
        match child.try_wait() {
            Ok(Some(exit)) => return exit,
            Ok(None) => (),
            Err(_) => {
                if !reported {
                    eprintln!("shutdown_child_observation_failed; retaining child ownership");
                    reported = true;
                }
            }
        }
        if let Some(request) = observer.current_request()
            && forwarded != Some(request.reason)
            && notify_owned_child(child, request.reason)
        {
            forwarded = Some(request.reason);
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}
/// Child is still owned and unreaped: its PID cannot be reused while signaling.
fn notify_owned_child(child: &mut std::process::Child, reason: ShutdownReason) -> bool {
    if !matches!(child.try_wait(), Ok(None)) {
        return false;
    }
    #[cfg(unix)]
    {
        #[allow(unsafe_code)]
        fn signal_owned(pid: u32, reason: ShutdownReason) -> bool {
            let Ok(pid) = i32::try_from(pid) else {
                return false;
            };
            // SAFETY: caller retains the live unreaped Child for this exact PID.
            let signal = if reason == ShutdownReason::Interrupt {
                libc::SIGINT
            } else {
                libc::SIGTERM
            };
            unsafe { libc::kill(pid, signal) == 0 }
        }
        signal_owned(child.id(), reason)
    }
    #[cfg(windows)]
    {
        crate::shutdown_signal::notify_owned_child(child.id(), reason)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (child, reason);
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn deadlines_share_one_budget_and_escalation_cannot_extend_it() {
        let now = Instant::now();
        let original = ShutdownRequest {
            reason: ShutdownReason::Interrupt,
            deadline: now + Duration::from_secs(60),
        };
        let os = ShutdownRequest {
            reason: ShutdownReason::SessionEnding,
            deadline: now + Duration::from_secs(2),
        };
        assert_eq!(
            earliest(Some(original), Some(os)).unwrap().deadline,
            os.deadline
        );
        assert_eq!(
            earliest(Some(os), Some(original)).unwrap().deadline,
            os.deadline
        );
    }
    #[test]
    fn deadline_child() {
        if std::env::var_os("VESTA_SHUTDOWN_DEADLINE_CHILD").is_none() {
            return;
        }
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let signal = crate::shutdown_signal::ShutdownSignal::register().unwrap();
            let watchdog = ExitWatchdog::arm(signal.observer()).unwrap();
            watchdog.begin(ShutdownRequest {
                reason: ShutdownReason::Terminate,
                deadline: Instant::now() + Duration::from_millis(100),
            });
            if std::env::var_os("VESTA_SHUTDOWN_BLOCK_STDERR").is_some() {
                eprintln!("{}", "x".repeat(1024 * 1024));
            }
            loop {
                std::thread::park();
            }
        });
    }
    #[test]
    fn blocked_cleanup_exits_uncleanly_within_process_budget() {
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "process_shutdown::tests::deadline_child",
                "--nocapture",
            ])
            .env("VESTA_SHUTDOWN_DEADLINE_CHILD", "1")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let until = Instant::now() + Duration::from_secs(5);
        loop {
            if child.try_wait().unwrap().is_some() {
                break;
            }
            if Instant::now() >= until {
                let _ = child.kill();
                let _ = child.wait();
                panic!("deadline failed to end its own child process");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let output = child.wait_with_output().unwrap();
        assert_eq!(output.status.code(), Some(1));
        assert!(String::from_utf8_lossy(&output.stderr).contains("shutdown_started"));
    }
    #[test]
    fn deadline_does_not_wait_for_a_full_stderr_pipe() {
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "process_shutdown::tests::deadline_child",
                "--nocapture",
            ])
            .env("VESTA_SHUTDOWN_DEADLINE_CHILD", "1")
            .env("VESTA_SHUTDOWN_BLOCK_STDERR", "1")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let until = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                assert_eq!(status.code(), Some(1));
                break;
            }
            if Instant::now() >= until {
                let _ = child.kill();
                let _ = child.wait();
                panic!("full stderr defeated deadline");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
