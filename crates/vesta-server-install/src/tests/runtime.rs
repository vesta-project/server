use super::super::runtime::{parse_output, probe, probe_command, RuntimeBinding};
use super::*;

fn owned_child() -> bool {
    let a: Vec<_> = std::env::args().collect();
    a.iter().any(|v| v == "--exact") && a.iter().any(|v| v.contains("tests::runtime::child_"))
}
#[test]
fn child_good() {
    if !owned_child() {
        return;
    }
    for k in [
        "JAVA_TOOL_OPTIONS",
        "_JAVA_OPTIONS",
        "JDK_JAVA_OPTIONS",
        "OPENJ9_JAVA_OPTIONS",
        "IBM_JAVA_OPTIONS",
                "CURSEFORGE_API_KEY",
                "VESTA_CURSEFORGE_API_KEY",
    ] {
        assert!(std::env::var_os(k).is_none());
    }
    eprintln!("openjdk version \"21.0.1\" 2023-10-17");
}
#[test]
fn child_stdout_flood() {
    if owned_child() {
        print!("{}", "x".repeat(32 * 1024));
    }
}
#[test]
fn child_stderr_flood() {
    if owned_child() {
        eprint!("{}", "x".repeat(32 * 1024));
    }
}
#[test]
fn child_hang() {
    if owned_child() {
        std::thread::sleep(Duration::from_secs(30));
    }
}
#[test]
fn child_failed() {
    if owned_child() {
        std::process::exit(9);
    }
}
#[test]
fn child_unparseable() {
    if owned_child() {
        eprintln!("custom runtime no version");
    }
}
#[test]
fn child_conflicting() {
    if owned_child() {
        println!("\njava version \"17.0.1\"");
        eprintln!("openjdk version \"21.0.1\"");
    }
}
async fn run_child(
    name: &str,
    r: &dyn ProgressReporter,
    duration: Duration,
    cap: usize,
) -> Result<u32> {
    let name = format!("tests::runtime::{}", name);
    probe_command(
        &std::env::current_exe().unwrap(),
        &["--exact", &name, "--nocapture", "--test-threads=1"],
        r,
        duration,
        cap,
    )
    .await
}
#[allow(unsafe_code)]
#[test]
fn child_option_injection_parent() {
    if !owned_child() {
        return;
    }
    // Environment changes occur only in this isolated fixture child, never the test runner.
    for key in [
        "JAVA_TOOL_OPTIONS",
        "_JAVA_OPTIONS",
        "JDK_JAVA_OPTIONS",
        "OPENJ9_JAVA_OPTIONS",
        "IBM_JAVA_OPTIONS",
                "CURSEFORGE_API_KEY",
                "VESTA_CURSEFORGE_API_KEY",
    ] {
        // SAFETY: this mutation is confined to the isolated exact-selector child process.
        unsafe {
            std::env::set_var(key, "must-not-reach-probe");
        }
    }
    let rt = tokio::runtime::Runtime::new().unwrap();
    assert_eq!(
        rt.block_on(run_child(
            "child_good",
            &SilentProgressReporter,
            Duration::from_secs(3),
            16384
        ))
        .unwrap(),
        21
    );
    for key in [
        "JAVA_TOOL_OPTIONS",
        "_JAVA_OPTIONS",
        "JDK_JAVA_OPTIONS",
        "OPENJ9_JAVA_OPTIONS",
        "IBM_JAVA_OPTIONS",
                "CURSEFORGE_API_KEY",
                "VESTA_CURSEFORGE_API_KEY",
    ] {
        assert_eq!(std::env::var(key).unwrap(), "must-not-reach-probe");
    }
    println!("\nopenjdk version \"21.0.1\"");
}
#[tokio::test]
async fn selected_probe_owns_child_and_clears_java_injection_variables() {
    assert_eq!(
        run_child(
            "child_good",
            &SilentProgressReporter,
            Duration::from_secs(3),
            16384
        )
        .await
        .unwrap(),
        21
    );
    assert_eq!(
        run_child(
            "child_option_injection_parent",
            &SilentProgressReporter,
            Duration::from_secs(5),
            16384
        )
        .await
        .unwrap(),
        21
    );
}
#[tokio::test]
async fn both_probe_streams_are_individually_bounded() {
    for name in ["child_stdout_flood", "child_stderr_flood"] {
        assert_eq!(
            run_child(name, &SilentProgressReporter, Duration::from_secs(3), 256)
                .await
                .unwrap_err(),
            InstallError::LimitExceeded
        );
    }
}
#[tokio::test]
async fn hanging_probe_has_deadline_and_cancellation_reaps_owned_child() {
    let start = tokio::time::Instant::now();
    assert_eq!(
        run_child(
            "child_hang",
            &SilentProgressReporter,
            Duration::from_millis(100),
            16384
        )
        .await
        .unwrap_err(),
        InstallError::Deadline
    );
    assert!(start.elapsed() < Duration::from_secs(3));
    let r = Reporter::default();
    let cancel = async {
        tokio::time::sleep(Duration::from_millis(100)).await;
        r.cancel.store(true, Ordering::SeqCst);
    };
    let (result, _) = tokio::join!(
        run_child("child_hang", &r, Duration::from_secs(5), 16384),
        cancel
    );
    assert_eq!(result.unwrap_err(), InstallError::Cancelled);
}
#[tokio::test]
async fn failed_unknown_and_conflicting_probes_fail_closed() {
    for name in ["child_failed", "child_unparseable", "child_conflicting"] {
        assert_eq!(
            run_child(name, &SilentProgressReporter, Duration::from_secs(3), 16384)
                .await
                .unwrap_err(),
            InstallError::RuntimeFailed
        );
    }
}
#[test]
fn java_parser_requires_recognized_unambiguous_utf8_output() {
    for (s, n) in [
        ("java version \"1.8.0_392\"", 8),
        ("openjdk version \"17.0.1\"", 17),
        ("openjdk version \"25-ea\"", 25),
    ] {
        assert_eq!(parse_output(s.as_bytes(), b"").unwrap(), n);
        assert_eq!(parse_output(b"", s.as_bytes()).unwrap(), n);
    }
    for s in [
        b"".as_slice(),
        b"version 21",
        b"java version \"7\"",
        b"java version \"100\"",
        b"java version \"unknown\"",
        b"java version \"21",
        b"\xff",
    ] {
        assert_eq!(
            parse_output(s, b"").unwrap_err(),
            InstallError::RuntimeFailed
        );
    }
}
#[tokio::test]
async fn dry_and_cancelled_probes_do_not_spawn_and_relative_or_missing_paths_reject() {
    let r = Reporter {
        dry: true,
        ..Default::default()
    };
    assert_eq!(
        run_child("child_failed", &r, Duration::from_secs(1), 16384)
            .await
            .unwrap_err(),
        InstallError::RuntimeUnknown
    );
    let r = Reporter::default();
    r.cancel.store(true, Ordering::SeqCst);
    assert_eq!(
        run_child("child_failed", &r, Duration::from_secs(1), 16384)
            .await
            .unwrap_err(),
        InstallError::Cancelled
    );
    assert!(matches!(
        probe(Path::new("relative-java"), &SilentProgressReporter).await,
        Err(InstallError::RuntimeFailed)
    ));
    let dir = tempfile::tempdir().unwrap();
    assert!(probe(&dir.path().join("missing"), &SilentProgressReporter)
        .await
        .is_err());
}
#[test]
fn runtime_snapshot_rechecks_selected_plain_file_bytes_and_mtime() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("runtime");
    std::fs::write(&path, b"inert").unwrap();
    let binding = RuntimeBinding::snapshot(&path, 21).unwrap();
    binding.recheck().unwrap();
    std::fs::write(&path, b"different").unwrap();
    assert_eq!(
        binding.recheck().unwrap_err(),
        InstallError::ApprovalMismatch
    );
    assert!(RuntimeBinding::snapshot(dir.path(), 21).is_err());
}
#[tokio::test]
async fn production_probe_refuses_already_expired_absolute_deadline() {
    assert_eq!(
        super::super::runtime::probe_until(
            &std::env::current_exe().unwrap(),
            &SilentProgressReporter,
            tokio::time::Instant::now() - Duration::from_secs(1),
        )
        .await
        .err()
        .unwrap(),
        InstallError::Deadline,
    );
}

#[tokio::test]
async fn planning_deadline_bounds_real_probe_admission_without_spawn() {
    let mut f = Fixture::new(Software::Paper).await;
    f.installer.fixture_java = None;
    f.installer.limits.total_ms = 200;
    let pool = super::super::execution::Pool::shared();
    let _first = pool.hold_wait().await;
    let _second = pool.hold_wait().await;
    assert_eq!(pool.available(), 0);
    let start = tokio::time::Instant::now();
    let result = tokio::time::timeout(Duration::from_secs(1), f.plan_result())
        .await
        .expect("planning must not start a fresh ten-second probe deadline");
    assert_eq!(result.unwrap_err(), InstallError::Deadline);
    assert!(start.elapsed() < Duration::from_secs(1));
    assert_eq!(pool.available(), 0);
    let requests = f.server.received_requests().await.unwrap();
    assert!(requests.iter().any(|r| r.url.path().ends_with("/builds")));
    assert!(!requests.iter().any(|r| r.url.path() == f.artifact_path));
    assert!(entries(f.root.path()).is_empty());
}

#[tokio::test]
async fn real_probe_path_inside_managed_root_is_refused_before_spawn_attempt() {
    let mut f = Fixture::new(Software::Paper).await;
    f.installer.fixture_java = None;
    let runtime = f.root.path().join("not-an-executable");
    std::fs::write(&runtime, b"inert pinned file").unwrap();
    assert_eq!(
        f.installer
            .plan(
                Software::Paper,
                &Selection::default(),
                &runtime,
                &SilentProgressReporter
            )
            .await
            .unwrap_err(),
        InstallError::PathRefused
    );
    assert_eq!(std::fs::read(&runtime).unwrap(), b"inert pinned file");
    assert!(!f
        .server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .any(|r| r.url.path() == f.artifact_path));
}

#[tokio::test]
async fn incompatible_runtime_fails_before_artifact_effects() {
    let mut f = Fixture::new(Software::Paper).await;
    f.installer.fixture_java = Some(17);
    assert_eq!(
        f.plan_result().await.unwrap_err(),
        InstallError::RuntimeIncompatible
    );
    assert!(entries(f.root.path()).is_empty());
    assert!(!f
        .server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .any(|r| r.url.path() == f.artifact_path));
}
