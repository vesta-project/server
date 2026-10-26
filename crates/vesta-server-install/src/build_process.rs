//! Owned, bounded process-family execution shared by installer build tools.
//! This is lifecycle ownership, not a sandbox for upstream build code.
use crate::{InstallError, Result, progress::ProgressReporter};
use std::{
    io::Read,
    path::PathBuf,
    process::{Child, Command},
    time::Duration,
};
use vesta_game_runtime::process::tree::ProcessTree;

pub(crate) fn heavy_slot() -> Result<tokio::sync::OwnedSemaphorePermit> {
    static HEAVY: std::sync::OnceLock<std::sync::Arc<tokio::sync::Semaphore>> =
        std::sync::OnceLock::new();
    HEAVY
        .get_or_init(|| std::sync::Arc::new(tokio::sync::Semaphore::new(1)))
        .clone()
        .try_acquire_owned()
        .map_err(|_| InstallError::Busy)
}
struct Running {
    child: Child,
    tree: ProcessTree,
    confirmed: bool,
}
#[derive(Clone, Copy)]
pub(crate) struct LogPolicy {
    pub limit: usize,
    pub reject_incomplete_decompilation: bool,
}
impl Drop for Running {
    fn drop(&mut self) {
        if !self.confirmed {
            let _ = self.tree.kill(&mut self.child);
            let _ = self.child.kill();
            // Keep the exact child owned through a bounded cleanup attempt, never look up a PID.
            let until = std::time::Instant::now() + Duration::from_secs(2);
            while std::time::Instant::now() < until {
                if self.child.try_wait().ok().flatten().is_some() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }
}
pub(crate) async fn run(
    mut command: Command,
    invocation_file: PathBuf,
    deadline: tokio::time::Instant,
    reporter: &dyn ProgressReporter,
    policy: LogPolicy,
    mut monitor: impl FnMut() -> Result<()>,
) -> Result<()> {
    crate::execution::check(deadline, reporter)?;
    let log_limit = policy.limit;
    if log_limit == 0 || log_limit > 8 * 1024 * 1024 {
        return Err(InstallError::InvalidInput);
    }
    let tree = ProcessTree::prepare().map_err(|_| InstallError::RuntimeFailed)?;
    ProcessTree::configure(&mut command);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        use windows_sys::Win32::System::Threading::{CREATE_NO_WINDOW, CREATE_SUSPENDED};
        command.creation_flags(CREATE_NO_WINDOW | CREATE_SUSPENDED);
    }
    let child = command.spawn().map_err(|_| InstallError::RuntimeFailed)?;
    let mut running = Running {
        child,
        tree,
        confirmed: false,
    };
    running
        .tree
        .attach(&running.child)
        .map_err(|_| InstallError::RuntimeFailed)?;
    let out = running
        .child
        .stdout
        .take()
        .ok_or(InstallError::RuntimeFailed)?;
    let err = running
        .child
        .stderr
        .take()
        .ok_or(InstallError::RuntimeFailed)?;
    let drain = move |mut pipe: Box<dyn Read + Send>, path: PathBuf| {
        tokio::task::spawn_blocking(move || {
            let mut log = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(path)
                .map_err(|_| InstallError::RuntimeFailed)?;
            let mut buffer = [0u8; 8192];
            let mut total = 0usize;
            let mut tail = Vec::new();
            loop {
                let n = pipe
                    .read(&mut buffer)
                    .map_err(|_| InstallError::RuntimeFailed)?;
                if n == 0 {
                    return Ok(());
                }
                total += n;
                if total > log_limit {
                    return Err(InstallError::LimitExceeded);
                }
                std::io::Write::write_all(&mut log, &buffer[..n])
                    .map_err(|_| InstallError::RuntimeFailed)?;
                if policy.reject_incomplete_decompilation {
                    tail.extend_from_slice(&buffer[..n]);
                    if [
                        b"OutOfMemoryError:".as_slice(),
                        b"couldn't be fully decompiled".as_slice(),
                    ]
                    .iter()
                    .any(|needle| tail.windows(needle.len()).any(|part| part == *needle))
                    {
                        return Err(InstallError::RuntimeFailed);
                    }
                    let start = tail.len().saturating_sub(32);
                    tail.copy_within(start.., 0);
                    tail.truncate(32);
                }
            }
        })
    };
    let mut stdout = Some(drain(
        Box::new(out),
        invocation_file.with_extension("stdout.log"),
    ));
    let mut stderr = Some(drain(
        Box::new(err),
        invocation_file.with_extension("stderr.log"),
    ));
    let mut measure_at = tokio::time::Instant::now();
    loop {
        crate::execution::check(deadline, reporter)?;
        if tokio::time::Instant::now() >= measure_at {
            monitor()?;
            measure_at = tokio::time::Instant::now() + Duration::from_secs(1);
        }
        for output in [&mut stdout, &mut stderr] {
            if output.as_ref().is_some_and(|task| task.is_finished()) {
                output
                    .take()
                    .ok_or(InstallError::RuntimeFailed)?
                    .await
                    .map_err(|_| InstallError::RuntimeFailed)??;
            }
        }
        if let Some(status) = vesta_game_runtime::process::tree::observe(&mut running.child)
            .map_err(|_| InstallError::RuntimeFailed)?
        {
            // On Unix observe keeps the group leader unreaped until the family kill.
            running
                .tree
                .kill(&mut running.child)
                .map_err(|_| InstallError::RuntimeFailed)?;
            running
                .child
                .wait()
                .map_err(|_| InstallError::RuntimeFailed)?;
            let drains = async {
                for output in [stdout.take(), stderr.take()].into_iter().flatten() {
                    output.await.map_err(|_| InstallError::RuntimeFailed)??;
                }
                Ok(())
            };
            crate::transfer::controlled(drains, deadline, reporter).await?;
            running.confirmed = true;

            return if status.success() {
                Ok(())
            } else {
                Err(InstallError::RuntimeFailed)
            };
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SilentProgressReporter;
    use std::process::Stdio;

    #[test]
    fn child() {
        let args: Vec<_> = std::env::args().collect();
        if !args.iter().any(|s| s == "--exact")
            || !args.iter().any(|s| s == "build_process::tests::child")
        {
            return;
        }
        if args.iter().any(|s| s == "flood") {
            print!("{}", "x".repeat(2 * 1024 * 1024));
        }
        if args.iter().any(|s| s == "incomplete") {
            use std::io::Write;
            print!("{}OutOfMem", "x".repeat(8190));
            std::io::stdout().flush().unwrap();
            std::thread::sleep(Duration::from_millis(30));
            println!("oryError: Java heap space");
            std::io::stdout().flush().unwrap();
            std::thread::sleep(Duration::from_secs(30));
        }
        if args.iter().any(|s| s == "hang") {
            std::thread::sleep(Duration::from_secs(30));
        }
        if args.iter().any(|s| s == "fail") {
            std::process::exit(7);
        }
    }
    fn child_command(mode: &str) -> Command {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "build_process::tests::child",
                "--nocapture",
                "--test-threads=1",
                "--skip",
                mode,
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command
    }
    #[tokio::test]
    async fn monitor_failures_and_deadlines_stop_owned_children() {
        for (mode, monitor_failure) in [
            ("hang", true),
            ("hang", false),
            ("fail", false),
            ("flood", false),
            ("incomplete", false),
        ] {
            let root = tempfile::tempdir().unwrap();
            let until = tokio::time::Instant::now()
                + Duration::from_millis(if mode == "hang" { 150 } else { 3000 });
            let result = run(
                child_command(mode),
                root.path().join("log"),
                until,
                &SilentProgressReporter,
                LogPolicy {
                    limit: 1024 * 1024,
                    reject_incomplete_decompilation: mode == "incomplete",
                },
                || {
                    if monitor_failure {
                        Err(InstallError::StorageLow)
                    } else {
                        Ok(())
                    }
                },
            )
            .await;
            let expected = if monitor_failure {
                InstallError::StorageLow
            } else if mode == "hang" {
                InstallError::Deadline
            } else if mode == "flood" {
                InstallError::LimitExceeded
            } else {
                InstallError::RuntimeFailed
            };
            assert_eq!(result, Err(expected), "{mode}");
        }
    }
    #[test]
    fn cpu_heavy_installs_share_one_per_process_slot() {
        let permit = heavy_slot().unwrap();
        assert!(matches!(heavy_slot(), Err(InstallError::Busy)));
        drop(permit);
        assert!(heavy_slot().is_ok());
    }
}
