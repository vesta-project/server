use super::super::execution::{self, Budget, Disposition, Faults, Invocation, Pool};
use super::*;
use std::{
    io::{Read as _, Write as _},
    sync::{atomic::AtomicUsize, Arc},
};
use tokio::time::Instant;

const CHILD: &str = "tests::execution::child_fixture";
const HOLDER: &str = "tests::execution::child_pipe_holder";
const SECRET: &str = "fixture-secret-must-not-escape";

pub(super) fn args(root: &Path, mode: &str, extra: &[&str]) -> Vec<String> {
    let mut args = vec![
        "--exact".into(),
        CHILD.into(),
        "--nocapture".into(),
        "--test-threads=1".into(),
    ];
    for value in std::iter::once(root.to_str().unwrap())
        .chain(std::iter::once(mode))
        .chain(extra.iter().copied())
    {
        args.push("--skip".into());
        args.push(value.into());
    }
    args
}
fn fixture_values(name: &str) -> Option<Vec<String>> {
    let args: Vec<_> = std::env::args().collect();
    if !args.iter().any(|v| v == "--exact") || !args.iter().any(|v| v == name) {
        return None;
    }
    Some(
        args.windows(2)
            .filter(|a| a[0] == "--skip")
            .map(|a| a[1].clone())
            .collect(),
    )
}
#[allow(unsafe_code)]
#[test]
fn child_fixture() {
    let Some(values) = fixture_values(CHILD) else {
        return;
    };
    let root = PathBuf::from(&values[0]);
    std::fs::write(root.join("ready"), b"owned").unwrap();
    match values[1].as_str() {
        "good" => {
            println!("{}", values.get(2).map(String::as_str).unwrap_or("literal"));
            eprintln!("inert stderr");
        }
        "stdout" => print!("{}", "x".repeat(32 * 1024)),
        "stderr" => eprint!("{}", "x".repeat(32 * 1024)),
        "both" => {
            let out = std::thread::spawn(|| {
                for _ in 0..128 {
                    print!("out ");
                }
                std::io::stdout().flush().unwrap();
            });
            for _ in 0..128 {
                eprint!("err ");
            }
            out.join().unwrap();
        }
        "hang" => std::thread::sleep(Duration::from_secs(20)),
        "failed" => {
            eprintln!("{}", SECRET);
            std::process::exit(9);
        }
        "environment" => {
            for key in [
                "JAVA_TOOL_OPTIONS",
                "_JAVA_OPTIONS",
                "JDK_JAVA_OPTIONS",
                "OPENJ9_JAVA_OPTIONS",
                "IBM_JAVA_OPTIONS",
            ] {
                assert!(std::env::var_os(key).is_none());
            }
            let mut input = Vec::new();
            std::io::stdin().read_to_end(&mut input).unwrap();
            assert!(input.is_empty());
        }
        "environment_parent" => {
            let keys = [
                "JAVA_TOOL_OPTIONS",
                "_JAVA_OPTIONS",
                "JDK_JAVA_OPTIONS",
                "OPENJ9_JAVA_OPTIONS",
                "IBM_JAVA_OPTIONS",
            ];
            for key in keys {
                // SAFETY: this exact-selector fixture mutates the environment only in its isolated child process.
                unsafe {
                    std::env::set_var(key, SECRET);
                }
            }
            let runtime = tokio::runtime::Runtime::new().unwrap();
            let inner = root.join("inner");
            std::fs::create_dir(&inner).unwrap();
            let result = runtime.block_on(run(
                &inner,
                "environment",
                &SilentProgressReporter,
                Pool::new(),
                Budget::new(Duration::from_secs(5), 16384).unwrap(),
            ));
            result.result.unwrap();
            for key in keys {
                assert_eq!(std::env::var(key).unwrap(), SECRET);
            }
        }
        "inherited" => {
            // This self-terminating holder must outlive its direct parent to test inherited pipes.
            // Waiting here would remove the fault; the outer fixture waits before directory cleanup.
            #[allow(clippy::zombie_processes)]
            let child = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    HOLDER,
                    "--nocapture",
                    "--test-threads=1",
                    "--skip",
                    root.to_str().unwrap(),
                ])
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::inherit())
                .stderr(std::process::Stdio::inherit())
                .spawn()
                .unwrap();
            std::fs::write(root.join("descendant.pid"), child.id().to_string()).unwrap();
            // The fixture holder self-terminates; no production process-tree promise is inferred.
        }
        "job" | "job_tamper" | "job_extra" => {
            assert_eq!(
                dunce::canonicalize(std::env::current_dir().unwrap()).unwrap(),
                dunce::canonicalize(&root).unwrap()
            );
            let mut input = Vec::new();
            std::io::stdin().read_to_end(&mut input).unwrap();
            assert!(input.is_empty());
            assert_eq!(
                std::fs::read("../inputs/input.bin").unwrap(),
                b"fixture input"
            );
            std::fs::write("../outputs/libs/output.bin", b"fixture output").unwrap();
            if values[1] == "job_tamper" {
                std::fs::write("../inputs/input.bin", b"tampered data").unwrap();
            }
            if values[1] == "job_extra" {
                std::fs::write("../outputs/extra.bin", b"extra").unwrap();
            }
        }
        other => panic!("unknown fixture mode: {}", other),
    }
    std::io::stdout().flush().unwrap();
    std::io::stderr().flush().unwrap();
    std::process::exit(0);
}
#[test]
fn child_pipe_holder() {
    let Some(values) = fixture_values(HOLDER) else {
        return;
    };
    let root = PathBuf::from(&values[0]);
    std::fs::write(root.join("holder.ready"), b"owned").unwrap();
    std::thread::sleep(Duration::from_secs(2));
    std::fs::write(root.join("holder.done"), b"owned").unwrap();
}

pub(super) async fn wait_for(mut condition: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(8), async {
        while !condition() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("owned fixture readiness/completion");
}
pub(super) fn invocation(root: &Path, mode: &str, pool: Arc<Pool>) -> Invocation {
    Invocation::fixture(
        &std::env::current_exe().unwrap(),
        args(root, mode, &[]),
        None,
        pool,
    )
    .unwrap()
}
async fn run(
    root: &Path,
    mode: &str,
    reporter: &dyn ProgressReporter,
    pool: Arc<Pool>,
    budget: Budget,
) -> execution::Outcome {
    execution::run(invocation(root, mode, pool), budget, reporter).await
}

#[tokio::test]
async fn cancellation_expiry_and_dry_run_refuse_before_spawn() {
    for (reporter, budget, expected) in [
        (
            Reporter {
                dry: true,
                ..Default::default()
            },
            Budget::new(Duration::from_secs(5), 1024).unwrap(),
            InstallError::RuntimeUnknown,
        ),
        (
            Reporter::default(),
            Budget::expired(),
            InstallError::Deadline,
        ),
        (
            Reporter {
                cancel: AtomicBool::new(true),
                ..Default::default()
            },
            Budget::new(Duration::from_secs(5), 1024).unwrap(),
            InstallError::Cancelled,
        ),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let spawns = Arc::new(AtomicUsize::new(0));
        let mut invocation = invocation(dir.path(), "good", Pool::new());
        let counter = spawns.clone();
        invocation.on_spawn = Some(Arc::new(move |_| {
            counter.fetch_add(1, Ordering::SeqCst);
        }));
        let outcome = execution::run(invocation, budget, &reporter).await;
        assert_eq!(outcome.result.unwrap_err(), expected);
        assert_eq!(outcome.disposition, Disposition::NotStarted);
        assert_eq!(spawns.load(Ordering::SeqCst), 0);
        assert!(entries(dir.path()).is_empty());
    }
}
#[test]
fn invocation_and_budget_bounds_are_validated() {
    for (duration, cap) in [
        (Duration::ZERO, 1),
        (Duration::from_secs(601), 1),
        (Duration::from_secs(1), 0),
        (Duration::from_secs(1), 1024 * 1024 + 1),
    ] {
        assert!(Budget::new(duration, cap).is_err());
    }
    for args in [
        vec!["a".into(); 257],
        vec!["a".repeat(4097)],
        vec!["\n".into()],
        vec!["a".repeat(4096); 17],
    ] {
        assert_eq!(
            Invocation::fixture(&std::env::current_exe().unwrap(), args, None, Pool::new())
                .err()
                .unwrap(),
            InstallError::InvalidInput
        );
    }
    assert!(Invocation::fixture(Path::new("relative"), vec![], None, Pool::new()).is_err());
}
#[tokio::test]
async fn capped_reader_accepts_exact_bytes_and_rejects_one_more() {
    assert_eq!(
        execution::read_output(&b"abcd"[..], 4).await.unwrap(),
        b"abcd"
    );
    assert_eq!(
        execution::read_output(&b"abcde"[..], 4).await.unwrap_err(),
        InstallError::LimitExceeded
    );
    assert_eq!(execution::read_output(&b""[..], 4).await.unwrap(), b"");
}
#[tokio::test]
async fn owned_stream_floods_are_independently_refused_and_children_reaped() {
    for mode in ["stdout", "stderr"] {
        let dir = tempfile::tempdir().unwrap();
        let pool = Pool::new();
        let outcome = run(
            dir.path(),
            mode,
            &SilentProgressReporter,
            pool.clone(),
            Budget::new(Duration::from_secs(5), 1024).unwrap(),
        )
        .await;
        assert_eq!(outcome.result.unwrap_err(), InstallError::LimitExceeded);
        assert_eq!(outcome.disposition, Disposition::Exited);
        assert_eq!(pool.available(), 2);
    }
}
#[tokio::test]
async fn concurrent_streams_are_drained_and_literal_argv_is_not_a_shell() {
    let dir = tempfile::tempdir().unwrap();
    let output = run(
        dir.path(),
        "both",
        &SilentProgressReporter,
        Pool::new(),
        Budget::new(Duration::from_secs(5), 16384).unwrap(),
    )
    .await
    .result
    .unwrap();
    assert!(String::from_utf8(output.stdout)
        .unwrap()
        .contains("out out"));
    assert!(String::from_utf8(output.stderr)
        .unwrap()
        .contains("err err"));
    let literal = "$(touch escaped) ; & | > escaped";
    let invocation = Invocation::fixture(
        &std::env::current_exe().unwrap(),
        args(dir.path(), "good", &[literal]),
        None,
        Pool::new(),
    )
    .unwrap();
    let output = execution::run(
        invocation,
        Budget::new(Duration::from_secs(5), 16384).unwrap(),
        &SilentProgressReporter,
    )
    .await
    .result
    .unwrap();
    assert!(String::from_utf8(output.stdout).unwrap().contains(literal));
    assert_eq!(entries(dir.path()), ["ready"]);
}
#[tokio::test]
async fn java_options_and_null_stdin_are_enforced_without_mutating_parent_environment() {
    let dir = tempfile::tempdir().unwrap();
    run(
        dir.path(),
        "environment_parent",
        &SilentProgressReporter,
        Pool::new(),
        Budget::new(Duration::from_secs(8), 16384).unwrap(),
    )
    .await
    .result
    .unwrap();
}
#[tokio::test]
async fn nonzero_exit_has_a_fixed_secret_free_error() {
    let dir = tempfile::tempdir().unwrap();
    let outcome = run(
        dir.path(),
        "failed",
        &SilentProgressReporter,
        Pool::new(),
        Budget::new(Duration::from_secs(5), 16384).unwrap(),
    )
    .await;
    assert_eq!(outcome.disposition, Disposition::Exited);
    let error = outcome.result.unwrap_err();
    assert_eq!(error, InstallError::RuntimeFailed);
    assert_eq!(error.to_string(), "server_install.runtime_failed");
    assert!(!format!("{:?}", error).contains(SECRET));
}
#[tokio::test]
async fn admission_waits_are_deadline_and_cancellation_bounded_without_spawn() {
    let pool = Pool::new();
    let _first = pool.hold();
    let _second = pool.hold();
    let dir = tempfile::tempdir().unwrap();
    let outcome = run(
        dir.path(),
        "good",
        &SilentProgressReporter,
        pool.clone(),
        Budget::new(Duration::from_millis(50), 1024).unwrap(),
    )
    .await;
    assert_eq!(outcome.result.unwrap_err(), InstallError::Deadline);
    assert_eq!(outcome.disposition, Disposition::NotStarted);
    let reporter = Reporter::default();
    let cancel = async {
        tokio::task::yield_now().await;
        reporter.cancel.store(true, Ordering::SeqCst);
    };
    let (outcome, _) = tokio::join!(
        run(
            dir.path(),
            "good",
            &reporter,
            pool,
            Budget::new(Duration::from_secs(5), 1024).unwrap()
        ),
        cancel
    );
    assert_eq!(outcome.result.unwrap_err(), InstallError::Cancelled);
    assert!(entries(dir.path()).is_empty());
}
#[tokio::test]
async fn pause_while_queued_releases_admission_for_unpaused_requests() {
    let dir = tempfile::tempdir().unwrap();
    let other = tempfile::tempdir().unwrap();
    let pool = Pool::new();
    let first = pool.hold();
    let second = pool.hold();
    let queued = Arc::new(AtomicBool::new(false));
    let reporter = Arc::new(Reporter::default());
    let mut invocation = invocation(dir.path(), "good", pool.clone());
    let barrier = queued.clone();
    invocation.on_admission = Some(Arc::new(move || {
        barrier.store(true, Ordering::SeqCst);
    }));
    let cloned = reporter.clone();
    let task = tokio::spawn(async move {
        execution::run(
            invocation,
            Budget::new(Duration::from_secs(5), 16384).unwrap(),
            &*cloned,
        )
        .await
    });
    wait_for(|| queued.load(Ordering::SeqCst)).await;
    reporter.pause.store(true, Ordering::SeqCst);
    drop(first);
    drop(second);
    wait_for(|| pool.available() == 2).await;
    run(
        other.path(),
        "good",
        &SilentProgressReporter,
        pool.clone(),
        Budget::new(Duration::from_secs(3), 16384).unwrap(),
    )
    .await
    .result
    .unwrap();
    assert!(!dir.path().join("ready").exists());
    reporter.cancel.store(true, Ordering::SeqCst);
    let outcome = task.await.unwrap();
    assert_eq!(outcome.disposition, Disposition::NotStarted);
    assert_eq!(outcome.result.unwrap_err(), InstallError::Cancelled);
    assert_eq!(pool.available(), 2);
}

#[tokio::test]
async fn running_child_deadline_reaps_owned_child_and_releases_admission() {
    let dir = tempfile::tempdir().unwrap();
    let pool = Pool::new();
    let mut invocation = invocation(dir.path(), "hang", pool.clone());
    #[cfg(windows)]
    let witness = NativeWitness::attach(&mut invocation);
    // The budget must outlast fixture process start-up under full-suite load; the child is
    // observed running before the deadline, which then (not cancellation) ends it.
    let budget = Budget::new(Duration::from_secs(4), 16384).unwrap();
    let deadline = budget.deadline;
    let task =
        tokio::spawn(
            async move { execution::run(invocation, budget, &SilentProgressReporter).await },
        );
    wait_for(|| dir.path().join("ready").exists()).await;
    assert!(
        Instant::now() < deadline,
        "child observed running before the deadline"
    );
    let outcome = task.await.unwrap();
    assert!(Instant::now() >= deadline);
    assert_eq!(outcome.result.unwrap_err(), InstallError::Deadline);
    assert_eq!(outcome.disposition, Disposition::Exited);
    assert_eq!(pool.available(), 2);
    #[cfg(windows)]
    witness.assert_exited();
}

#[tokio::test]
async fn paused_admission_never_spawns_and_deadline_is_not_reset() {
    let dir = tempfile::tempdir().unwrap();
    let pool = Pool::new();
    let reporter = Reporter {
        pause: AtomicBool::new(true),
        ..Default::default()
    };
    let outcome = run(
        dir.path(),
        "good",
        &reporter,
        pool.clone(),
        Budget::new(Duration::from_millis(50), 1024).unwrap(),
    )
    .await;
    assert_eq!(outcome.result.unwrap_err(), InstallError::Deadline);
    assert_eq!(outcome.disposition, Disposition::NotStarted);
    assert_eq!(pool.available(), 2);
    assert!(entries(dir.path()).is_empty());
}

#[cfg(windows)]
pub(super) struct NativeWitness {
    handle: Mutex<Option<std::os::windows::io::OwnedHandle>>,
}
#[cfg(windows)]
impl NativeWitness {
    #[cfg_attr(windows, allow(unsafe_code))]
    pub(super) fn attach(invocation: &mut Invocation) -> Arc<Self> {
        use std::os::windows::io::BorrowedHandle;
        let witness = Arc::new(Self {
            handle: Mutex::new(None),
        });
        let cloned = witness.clone();
        invocation.on_spawn = Some(Arc::new(move |child| {
            // SAFETY: the callback borrows the live child handle and immediately clones it.
            let handle = unsafe { BorrowedHandle::borrow_raw(child.raw_handle().unwrap()) }
                .try_clone_to_owned()
                .unwrap();
            *cloned.handle.lock().unwrap() = Some(handle);
        }));
        witness
    }
    #[cfg_attr(windows, allow(unsafe_code))]
    pub(super) fn assert_exited(&self) {
        use std::os::windows::io::AsRawHandle;
        let handle = self.handle.lock().unwrap();
        // SAFETY: the handle is an owned clone of the still-valid child process handle.
        let result = unsafe {
            windows_sys::Win32::System::Threading::WaitForSingleObject(
                handle
                    .as_ref()
                    .expect("spawned owned child handle")
                    .as_raw_handle(),
                5000,
            )
        };
        assert_eq!(
            result,
            windows_sys::Win32::Foundation::WAIT_OBJECT_0,
            "native owned child has exited"
        );
    }
}
#[cfg(windows)]
impl Drop for NativeWitness {
    #[cfg_attr(windows, allow(unsafe_code))]
    fn drop(&mut self) {
        use std::os::windows::io::AsRawHandle;
        // Assertion failures (including intentional mutations) must not race TempDir cleanup.
        if let Some(handle) = self
            .handle
            .get_mut()
            .unwrap_or_else(|p| p.into_inner())
            .as_ref()
        {
            // SAFETY: this is the still-owned process handle cloned for the child witness.
            unsafe {
                windows_sys::Win32::System::Threading::WaitForSingleObject(
                    handle.as_raw_handle(),
                    25000,
                );
            }
        }
    }
}

#[tokio::test]
async fn running_cancellation_confirms_exit_and_releases_admission() {
    let dir = tempfile::tempdir().unwrap();
    let pool = Pool::new();
    let reporter = Arc::new(Reporter::default());
    let mut invocation = invocation(dir.path(), "hang", pool.clone());
    #[cfg(windows)]
    let witness = NativeWitness::attach(&mut invocation);
    let cloned = reporter.clone();
    let task = tokio::spawn(async move {
        execution::run(
            invocation,
            Budget::new(Duration::from_secs(10), 16384).unwrap(),
            &*cloned,
        )
        .await
    });
    wait_for(|| dir.path().join("ready").exists()).await;
    assert_eq!(pool.available(), 1);
    reporter.cancel.store(true, Ordering::SeqCst);
    let outcome = task.await.unwrap();
    assert_eq!(outcome.result.unwrap_err(), InstallError::Cancelled);
    assert_eq!(outcome.disposition, Disposition::Exited);
    assert_eq!(pool.available(), 2);
    #[cfg(windows)]
    witness.assert_exited();
}
#[tokio::test]
async fn abort_hands_the_owned_child_to_a_reaper_before_releasing_admission() {
    let dir = tempfile::tempdir().unwrap();
    let pool = Pool::new();
    let mut invocation = invocation(dir.path(), "hang", pool.clone());
    #[cfg(windows)]
    let witness = NativeWitness::attach(&mut invocation);
    let task = tokio::spawn(async move {
        execution::run(
            invocation,
            Budget::new(Duration::from_secs(10), 16384).unwrap(),
            &SilentProgressReporter,
        )
        .await
    });
    wait_for(|| dir.path().join("ready").exists()).await;
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    #[cfg(windows)]
    witness.assert_exited();
    wait_for(|| pool.available() == 2).await;
}
#[tokio::test]
async fn unconfirmed_reap_and_refused_handoff_quarantine_admission() {
    let dir = tempfile::tempdir().unwrap();
    let pool = Pool::new();
    let reporter = Arc::new(Reporter::default());
    let mut invocation = invocation(dir.path(), "hang", pool.clone());
    invocation.faults = Faults {
        unconfirmed_reap: true,
        refuse_handoff: true,
        ..Default::default()
    };
    #[cfg(windows)]
    let witness = NativeWitness::attach(&mut invocation);
    let cloned = reporter.clone();
    let task = tokio::spawn(async move {
        execution::run(
            invocation,
            Budget::new(Duration::from_secs(10), 16384).unwrap(),
            &*cloned,
        )
        .await
    });
    wait_for(|| dir.path().join("ready").exists()).await;
    reporter.cancel.store(true, Ordering::SeqCst);
    let outcome = task.await.unwrap();
    assert_eq!(outcome.result.unwrap_err(), InstallError::Cancelled);
    assert_eq!(outcome.disposition, Disposition::Unconfirmed);
    assert_eq!(pool.available(), 1);
    #[cfg(windows)]
    witness.assert_exited();
}
#[cfg_attr(windows, allow(unsafe_code))]
#[tokio::test]
async fn inherited_pipe_after_direct_child_exit_cannot_extend_the_deadline() {
    let dir = tempfile::tempdir().unwrap();
    let reporter = Arc::new(Reporter::default());
    let pool = Pool::new();
    let task = tokio::spawn({
        let root = dir.path().to_path_buf();
        let cloned = reporter.clone();
        async move {
            run(
                &root,
                "inherited",
                &*cloned,
                pool,
                Budget::new(Duration::from_secs(1), 16384).unwrap(),
            )
            .await
        }
    });
    wait_for(|| dir.path().join("holder.ready").exists()).await;
    let start = Instant::now();
    let outcome = task.await.unwrap();
    assert_eq!(outcome.result.unwrap_err(), InstallError::Deadline);
    assert_eq!(outcome.disposition, Disposition::Exited);
    assert!(start.elapsed() < Duration::from_secs(3));
    wait_for(|| dir.path().join("holder.done").exists()).await;
    #[cfg(windows)]
    {
        use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
        let pid: u32 = std::fs::read_to_string(dir.path().join("descendant.pid"))
            .unwrap()
            .parse()
            .unwrap();
        // Wait-only on the ID recorded by our fixture. Never kill an arbitrary PID.
        // SAFETY: the PID was written by our owned fixture child; this requests only synchronization access.
        let raw = unsafe {
            windows_sys::Win32::System::Threading::OpenProcess(
                windows_sys::Win32::System::Threading::PROCESS_SYNCHRONIZE,
                0,
                pid,
            )
        };
        if !raw.is_null() {
            let handle = unsafe { OwnedHandle::from_raw_handle(raw) };
            assert_eq!(
                // SAFETY: the handle is owned and valid until this wait completes.
                unsafe {
                    windows_sys::Win32::System::Threading::WaitForSingleObject(
                        handle.as_raw_handle(),
                        5000,
                    )
                },
                windows_sys::Win32::Foundation::WAIT_OBJECT_0
            );
        }
    }
}
