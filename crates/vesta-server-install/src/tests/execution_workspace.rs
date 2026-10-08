use super::super::{
    execution::{self, Budget, Disposition, Faults, Invocation, Pool},
    execution_workspace::{DeclaredFile, Workspace},
};
use super::*;
use std::sync::Arc;

struct JobFixture {
    root: TempDir,
    installer: ServerInstaller,
}
impl JobFixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let base = root.path().join("managed");
        std::fs::create_dir(&base).unwrap();
        let mut installer = ServerInstaller::new(
            InstallRoots::new(&base, &[]).unwrap(),
            TransportConfig::direct(),
        )
        .unwrap();
        installer.coordinator = install::Coordinator::new();
        installer.fixture_space = Some((base.clone(), 1 << 40));
        installer.limits.artifact_bytes = 65536;
        installer.limits.total_bytes = 131072;
        installer.limits.total_ms = 15000;
        Self { root, installer }
    }
    fn budget(&self) -> Budget {
        Budget::new(Duration::from_secs(10), 16384).unwrap()
    }
    async fn workspace(&self, budget: &Budget) -> Workspace {
        let mut workspace = Workspace::create(
            &self.installer,
            vec![file("input.bin", b"fixture input")],
            vec![file("libs/output.bin", b"fixture output")],
            budget,
            &SilentProgressReporter,
        )
        .await
        .unwrap();
        workspace
            .seed_input(0, b"fixture input", budget, &SilentProgressReporter)
            .await
            .unwrap();
        workspace
    }
}
fn file(path: &str, bytes: &[u8]) -> DeclaredFile {
    DeclaredFile::new(
        path,
        bytes.len() as u64,
        PublishedDigest::new(DigestAlgorithm::Sha256, model::sha256(bytes)).unwrap(),
    )
    .unwrap()
}
fn workspace_result_error(result: Result<Workspace>) -> InstallError {
    result.err().expect("refused workspace")
}
fn job(workspace: Workspace, mode: &str, pool: Arc<Pool>) -> Invocation {
    let args = super::execution::args(workspace.cwd(), mode, &[]);
    Invocation::fixture(
        &std::env::current_exe().unwrap(),
        args,
        Some(workspace),
        pool,
    )
    .unwrap()
}

#[tokio::test]
async fn failed_spawn_cleans_only_known_prestart_files_and_releases_protection() {
    let mut f = JobFixture::new();
    f.installer.fixture_space.as_mut().unwrap().1 = f.installer.limits.reserve_bytes + 27;
    let budget = f.budget();
    let workspace = f.workspace(&budget).await;
    let directory = workspace.directory().to_path_buf();
    let runtime = f.root.path().join("not-an-executable");
    std::fs::write(&runtime, b"inert regular file").unwrap();
    let pool = Pool::new();
    let invocation = Invocation::fixture(&runtime, vec![], Some(workspace), pool.clone()).unwrap();
    let outcome = execution::run(invocation, budget, &SilentProgressReporter).await;
    assert_eq!(outcome.disposition, Disposition::NotStarted);
    assert_eq!(outcome.result.unwrap_err(), InstallError::RuntimeFailed);
    assert!(!directory.exists());
    assert_eq!(pool.available(), 2);
    let _first = f.installer.coordinator.slot().unwrap();
    let _second = f.installer.coordinator.slot().unwrap();
    let (volume, free) = f.installer.space().unwrap();
    let _reservation = f
        .installer
        .coordinator
        .reserve(volume, free, 27, f.installer.limits.reserve_bytes)
        .unwrap();
    assert_eq!(std::fs::read(runtime).unwrap(), b"inert regular file");
}

#[tokio::test]
async fn undeclared_input_files_and_directories_refuse_before_spawn() {
    for directory_entry in [false, true] {
        let f = JobFixture::new();
        let budget = f.budget();
        let workspace = f.workspace(&budget).await;
        let directory = workspace.directory().to_path_buf();
        let extra = directory.join("inputs").join("undeclared");
        if directory_entry {
            std::fs::create_dir(&extra).unwrap();
        } else {
            std::fs::write(&extra, b"not declared").unwrap();
        }
        let outcome = execution::run(
            job(workspace, "job", Pool::new()),
            budget,
            &SilentProgressReporter,
        )
        .await;
        assert_eq!(outcome.disposition, Disposition::NotStarted);
        assert_eq!(outcome.result.unwrap_err(), InstallError::PathRefused);
        assert!(!directory.join("work").join("ready").exists());
        assert!(extra.exists());
    }
}

#[tokio::test]
async fn empty_input_declarations_do_not_skip_root_validation() {
    for replace_with_file in [false, true] {
        let f = JobFixture::new();
        let budget = f.budget();
        let workspace = Workspace::create(
            &f.installer,
            vec![],
            vec![file("output", b"x")],
            &budget,
            &SilentProgressReporter,
        )
        .await
        .unwrap();
        let input = workspace.directory().join("inputs");
        std::fs::remove_dir(&input).unwrap();
        if replace_with_file {
            std::fs::write(&input, b"not a directory").unwrap();
        }
        assert_eq!(
            workspace
                .verify_inputs(budget.deadline, &SilentProgressReporter)
                .await
                .unwrap_err(),
            InstallError::PathRefused
        );
    }
}

#[tokio::test]
async fn same_size_mtime_path_replacement_cannot_substitute_verified_handle_bytes() {
    use super::super::execution_workspace::verify_replacement_fixture;
    for hard_link in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("declared");
        let moved = root.path().join("original");
        let target = root.path().join("protected");
        std::fs::write(&path, b"fixture output").unwrap();
        std::fs::write(&target, b"tampered bytes").unwrap();
        let modified = std::fs::metadata(&path).unwrap().modified().unwrap();
        let replace = |path: &Path| {
            std::fs::rename(path, &moved).unwrap();
            if hard_link {
                std::fs::hard_link(&target, path).unwrap();
            } else {
                std::fs::write(path, b"tampered bytes").unwrap();
            }
            std::fs::OpenOptions::new()
                .write(true)
                .open(path)
                .unwrap()
                .set_times(std::fs::FileTimes::new().set_modified(modified))
                .unwrap();
            assert_eq!(
                std::fs::metadata(path).unwrap().modified().unwrap(),
                modified
            );
        };
        let result =
            verify_replacement_fixture(&path, &file("declared", b"fixture output"), &replace).await;
        assert_eq!(
            result.err().unwrap(),
            if hard_link {
                InstallError::PathRefused
            } else {
                InstallError::ArtifactChanged
            }
        );
        assert_eq!(std::fs::read(&moved).unwrap(), b"fixture output");
        assert_eq!(std::fs::read(&path).unwrap(), b"tampered bytes");
        assert_eq!(std::fs::read(&target).unwrap(), b"tampered bytes");
    }
}

#[tokio::test]
async fn abort_with_workspace_reaps_before_releasing_slots_and_reservation() {
    let mut f = JobFixture::new();
    f.installer.fixture_space.as_mut().unwrap().1 = f.installer.limits.reserve_bytes + 27;
    let budget = f.budget();
    let workspace = f.workspace(&budget).await;
    let directory = workspace.directory().to_path_buf();
    let pool = Pool::new();
    let mut invocation = job(workspace, "hang", pool.clone());
    #[cfg(windows)]
    let witness = super::execution::NativeWitness::attach(&mut invocation);
    let task =
        tokio::spawn(
            async move { execution::run(invocation, budget, &SilentProgressReporter).await },
        );
    super::execution::wait_for(|| directory.join("work").join("ready").exists()).await;
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    #[cfg(windows)]
    witness.assert_exited();
    super::execution::wait_for(|| pool.available() == 2).await;
    let _first = f.installer.coordinator.slot().unwrap();
    let _second = f.installer.coordinator.slot().unwrap();
    let (volume, free) = f.installer.space().unwrap();
    let _reservation = f
        .installer
        .coordinator
        .reserve(volume, free, 27, f.installer.limits.reserve_bytes)
        .unwrap();
    assert!(directory.join("inputs").join("input.bin").exists());
    assert!(!f.installer.roots.base.join("instances").exists());
}

#[cfg(windows)]
#[tokio::test]
async fn runtime_shutdown_drops_owned_reaper_and_quarantines_job_protection() {
    let mut f = JobFixture::new();
    f.installer.fixture_space.as_mut().unwrap().1 = f.installer.limits.reserve_bytes + 27;
    let budget = f.budget();
    let workspace = f.workspace(&budget).await;
    let directory = workspace.directory().to_path_buf();
    let pool = Pool::new();
    let mut invocation = job(workspace, "hang", pool.clone());
    invocation.faults.stall_reaper = true;
    let witness = super::execution::NativeWitness::attach(&mut invocation);
    let ready = directory.join("work").join("ready");
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async move {
            let task = tokio::spawn(async move {
                execution::run(invocation, budget, &SilentProgressReporter).await
            });
            super::execution::wait_for(|| ready.exists()).await;
            task.abort();
            assert!(task.await.unwrap_err().is_cancelled());
            tokio::task::yield_now().await;
        });
        // The test-only stalled reaper cannot confirm exit; shutdown must run Pending::drop.
        drop(runtime);
    })
    .join()
    .unwrap();
    witness.assert_exited();
    assert_eq!(pool.available(), 1);
    assert!(directory.join("inputs").join("input.bin").exists());
    let _other = f.installer.coordinator.slot().unwrap();
    assert_eq!(
        f.installer.coordinator.slot().err().unwrap(),
        InstallError::Busy
    );
    let (volume, free) = f.installer.space().unwrap();
    assert_eq!(
        f.installer
            .coordinator
            .reserve(volume, free, 1, f.installer.limits.reserve_bytes)
            .err()
            .unwrap(),
        InstallError::StorageLow
    );
}

#[cfg(any(windows, unix))]
#[tokio::test]
async fn empty_declared_input_root_cannot_be_replaced_by_a_native_link() {
    let f = JobFixture::new();
    let budget = f.budget();
    let workspace = Workspace::create(
        &f.installer,
        vec![],
        vec![file("output", b"x")],
        &budget,
        &SilentProgressReporter,
    )
    .await
    .unwrap();
    let target = f.root.path().join("protected");
    std::fs::create_dir(&target).unwrap();
    std::fs::write(target.join("sentinel"), b"preserve").unwrap();
    let input = workspace.directory().join("inputs");
    std::fs::remove_dir(&input).unwrap();
    #[cfg(windows)]
    {
        let result = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(&input)
            .arg(&target)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "owned input junction setup: {:?}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
    #[cfg(unix)]
    std::os::unix::fs::symlink(&target, &input).unwrap();
    assert_eq!(
        workspace
            .verify_inputs(budget.deadline, &SilentProgressReporter)
            .await
            .unwrap_err(),
        InstallError::PathRefused
    );
    assert_eq!(std::fs::read(target.join("sentinel")).unwrap(), b"preserve");
    #[cfg(windows)]
    std::fs::remove_dir(input).unwrap();
    #[cfg(unix)]
    std::fs::remove_file(input).unwrap();
}

#[test]
fn declared_paths_refuse_escape_aliases_devices_and_protected_names() {
    for path in [
        "",
        "/outside",
        "../outside",
        "a/../b",
        "a//b",
        "a\\b",
        "C:/outside",
        "file:stream",
        "CON.txt",
        "nul",
        "COM1.bin",
        "LPT9.bin",
        "control/receipt.json",
        "STATE/db",
        ".git/config",
        "a.",
        "a/b ",
        "a\nb",
        "a/%20",
        "a/é",
    ] {
        assert_eq!(
            DeclaredFile::new(
                path,
                1,
                PublishedDigest::new(DigestAlgorithm::Sha256, model::sha256(b"x")).unwrap()
            )
            .err()
            .unwrap(),
            InstallError::PathRefused,
            "{}",
            path
        );
    }
    // Consecutive dots inside an ordinary component are not traversal (original policy).
    assert!(DeclaredFile::new(
        "cache/v1..backup.jar",
        1,
        PublishedDigest::new(DigestAlgorithm::Sha256, model::sha256(b"x")).unwrap()
    )
    .is_ok());
    assert!(DeclaredFile::new(
        "libs/a-1.2.jar",
        1,
        PublishedDigest::new(DigestAlgorithm::Sha256, model::sha256(b"x")).unwrap()
    )
    .is_ok());
    assert!(DeclaredFile::new(
        &vec!["a"; 17].join("/"),
        1,
        PublishedDigest::new(DigestAlgorithm::Sha256, model::sha256(b"x")).unwrap()
    )
    .is_err());
    assert!(DeclaredFile::new(
        &"a".repeat(129),
        1,
        PublishedDigest::new(DigestAlgorithm::Sha256, model::sha256(b"x")).unwrap()
    )
    .is_err());
}
#[tokio::test]
async fn conflicting_duplicate_and_case_aliased_declarations_refuse_before_writes() {
    for outputs in [
        vec![file("x", b"x"), file("x", b"x")],
        vec![file("x", b"x"), file("X", b"x")],
        vec![file("x", b"x"), file("x/y", b"x")],
        vec![file("Lib/x", b"x"), file("lib/y", b"x")],
    ] {
        let f = JobFixture::new();
        assert_eq!(
            workspace_result_error(
                Workspace::create(
                    &f.installer,
                    vec![],
                    outputs,
                    &f.budget(),
                    &SilentProgressReporter
                )
                .await
            ),
            InstallError::PathRefused
        );
        assert!(entries(&f.installer.roots.base).is_empty());
    }
}
#[tokio::test]
async fn declaration_entry_file_and_aggregate_caps_refuse_before_writes() {
    let mut f = JobFixture::new();
    f.installer.limits.max_entries = 2;
    assert_eq!(
        workspace_result_error(
            Workspace::create(
                &f.installer,
                vec![],
                vec![file("a", b"a"), file("b", b"b"), file("c", b"c")],
                &f.budget(),
                &SilentProgressReporter
            )
            .await
        ),
        InstallError::LimitExceeded
    );
    assert_eq!(
        workspace_result_error(
            Workspace::create(
                &f.installer,
                vec![],
                vec![],
                &f.budget(),
                &SilentProgressReporter
            )
            .await
        ),
        InstallError::LimitExceeded
    );
    let oversized = DeclaredFile::new(
        "x",
        65537,
        PublishedDigest::new(DigestAlgorithm::Sha256, model::sha256(b"x")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        workspace_result_error(
            Workspace::create(
                &f.installer,
                vec![],
                vec![oversized],
                &f.budget(),
                &SilentProgressReporter
            )
            .await
        ),
        InstallError::LimitExceeded
    );
    f.installer.limits.total_bytes = 3;
    assert_eq!(
        workspace_result_error(
            Workspace::create(
                &f.installer,
                vec![file("input", b"xx")],
                vec![file("output", b"xx")],
                &f.budget(),
                &SilentProgressReporter
            )
            .await
        ),
        InstallError::LimitExceeded
    );
    assert!(entries(&f.installer.roots.base).is_empty());
}
#[tokio::test]
async fn pre_execution_dry_cancel_and_expiry_have_no_directory_effects() {
    for (reporter, budget, expected) in [
        (
            Reporter {
                dry: true,
                ..Default::default()
            },
            Budget::new(Duration::from_secs(10), 16384).unwrap(),
            InstallError::RuntimeUnknown,
        ),
        (
            Reporter {
                cancel: AtomicBool::new(true),
                ..Default::default()
            },
            Budget::new(Duration::from_secs(10), 16384).unwrap(),
            InstallError::Cancelled,
        ),
        (
            Reporter::default(),
            Budget::expired(),
            InstallError::Deadline,
        ),
    ] {
        let f = JobFixture::new();
        assert_eq!(
            workspace_result_error(
                Workspace::create(
                    &f.installer,
                    vec![],
                    vec![file("output", b"x")],
                    &budget,
                    &reporter
                )
                .await
            ),
            expected
        );
        assert!(entries(&f.installer.roots.base).is_empty());
    }
}
#[tokio::test]
async fn workspace_creation_is_exclusive_and_never_adopts_a_collision() {
    let f = JobFixture::new();
    let budget = f.budget();
    let id = "job_11111111111111111111111111111111";
    let workspace = Workspace::collision(
        &f.installer,
        vec![],
        vec![file("output", b"x")],
        &budget,
        &SilentProgressReporter,
        id,
    )
    .await
    .unwrap();
    let before = entries(workspace.directory());
    assert_eq!(
        workspace_result_error(
            Workspace::collision(
                &f.installer,
                vec![],
                vec![file("output", b"x")],
                &budget,
                &SilentProgressReporter,
                id
            )
            .await
        ),
        InstallError::DestinationExists
    );
    assert_eq!(before, entries(workspace.directory()));
    let directory = workspace.directory().to_path_buf();
    drop(workspace);
    assert!(!directory.exists());
}
#[tokio::test]
async fn unknown_and_low_space_refuse_before_creating_job_files() {
    for error in [InstallError::StorageUnknown, InstallError::StorageLow] {
        let mut f = JobFixture::new();
        if error == InstallError::StorageUnknown {
            f.installer.fixture_space_error = Some(error);
        } else {
            f.installer.fixture_space.as_mut().unwrap().1 = f.installer.limits.reserve_bytes;
        }
        assert_eq!(
            workspace_result_error(
                Workspace::create(
                    &f.installer,
                    vec![],
                    vec![file("output", b"x")],
                    &f.budget(),
                    &SilentProgressReporter
                )
                .await
            ),
            error
        );
        assert!(entries(&f.installer.roots.base).is_empty());
    }
}
#[tokio::test]
async fn job_reservations_cannot_double_spend_and_unstarted_cleanup_releases_them() {
    let mut f = JobFixture::new();
    f.installer.fixture_space.as_mut().unwrap().1 = f.installer.limits.reserve_bytes + 27;
    let budget = f.budget();
    let workspace = f.workspace(&budget).await;
    assert_eq!(
        workspace_result_error(
            Workspace::create(
                &f.installer,
                vec![file("input.bin", b"fixture input")],
                vec![file("libs/output.bin", b"fixture output")],
                &budget,
                &SilentProgressReporter
            )
            .await
        ),
        InstallError::StorageLow
    );
    let directory = workspace.directory().to_path_buf();
    drop(workspace);
    assert!(!directory.exists());
    let replacement = f.workspace(&budget).await;
    drop(replacement);
}
#[tokio::test]
async fn input_pins_and_exclusive_input_files_are_required_before_execution() {
    let f = JobFixture::new();
    let budget = f.budget();
    let mut workspace = Workspace::create(
        &f.installer,
        vec![file("input.bin", b"fixture input")],
        vec![file("output.bin", b"x")],
        &budget,
        &SilentProgressReporter,
    )
    .await
    .unwrap();
    assert_eq!(
        workspace
            .verify_inputs(budget.deadline, &SilentProgressReporter)
            .await
            .unwrap_err(),
        InstallError::PathRefused
    );
    workspace
        .seed_input(0, b"fixture input", &budget, &SilentProgressReporter)
        .await
        .unwrap();
    workspace
        .verify_inputs(budget.deadline, &SilentProgressReporter)
        .await
        .unwrap();
    assert_eq!(
        workspace
            .seed_input(0, b"fixture input", &budget, &SilentProgressReporter)
            .await
            .unwrap_err(),
        InstallError::DestinationExists
    );
    std::fs::write(
        workspace.directory().join("inputs/input.bin"),
        b"tampered data",
    )
    .unwrap();
    assert_eq!(
        workspace
            .verify_inputs(budget.deadline, &SilentProgressReporter)
            .await
            .unwrap_err(),
        InstallError::ArtifactChanged
    );
}
#[tokio::test]
async fn legacy_and_strong_published_input_digests_are_checked_independently_of_observed_sha256() {
    for algorithm in [
        DigestAlgorithm::Sha256,
        DigestAlgorithm::Sha1,
        DigestAlgorithm::Md5,
    ] {
        let f = JobFixture::new();
        let budget = f.budget();
        let input = DeclaredFile::new(
            "input.bin",
            13,
            PublishedDigest::new(algorithm, digest(algorithm, b"fixture input")).unwrap(),
        )
        .unwrap();
        let mut workspace = Workspace::create(
            &f.installer,
            vec![input],
            vec![file("output.bin", b"x")],
            &budget,
            &SilentProgressReporter,
        )
        .await
        .unwrap();
        workspace
            .seed_input(0, b"fixture input", &budget, &SilentProgressReporter)
            .await
            .unwrap();
        workspace
            .verify_inputs(budget.deadline, &SilentProgressReporter)
            .await
            .unwrap();
        std::fs::write(
            workspace.directory().join("inputs/input.bin"),
            b"tampered data",
        )
        .unwrap();
        assert_eq!(
            workspace
                .verify_inputs(budget.deadline, &SilentProgressReporter)
                .await
                .unwrap_err(),
            InstallError::ArtifactChanged
        );
    }
}
#[tokio::test]
async fn output_digest_size_presence_and_undeclared_entries_are_verified() {
    for mode in [
        "missing",
        "digest",
        "short",
        "long",
        "extra",
        "extra_directory",
    ] {
        let f = JobFixture::new();
        let budget = f.budget();
        let workspace = f.workspace(&budget).await;
        let output = workspace.directory().join("outputs/libs/output.bin");
        match mode {
            "missing" => {}
            "digest" => std::fs::write(&output, b"corrupt output").unwrap(),
            "short" => std::fs::write(&output, b"short").unwrap(),
            "long" => std::fs::write(&output, b"fixture output!").unwrap(),
            "extra" => {
                std::fs::write(&output, b"fixture output").unwrap();
                std::fs::write(workspace.directory().join("outputs/extra.bin"), b"x").unwrap();
            }
            "extra_directory" => {
                std::fs::write(&output, b"fixture output").unwrap();
                std::fs::create_dir(workspace.directory().join("outputs/unknown")).unwrap();
            }
            _ => unreachable!(),
        }
        assert!(
            workspace
                .verify_after(budget.deadline, &SilentProgressReporter)
                .await
                .is_err(),
            "{}",
            mode
        );
        assert!(workspace.verified().is_none());
    }
}
#[tokio::test]
async fn verified_outputs_preserve_original_pins_and_record_observed_sha256() {
    let f = JobFixture::new();
    let budget = f.budget();
    let workspace = f.workspace(&budget).await;
    std::fs::write(
        workspace.directory().join("outputs/libs/output.bin"),
        b"fixture output",
    )
    .unwrap();
    workspace
        .verify_after(budget.deadline, &SilentProgressReporter)
        .await
        .unwrap();
    let files = workspace.verified().unwrap();
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].path, "libs/output.bin");
    assert_eq!(files[0].bytes, 14);
    assert_eq!(files[0].sha256, model::sha256(b"fixture output"));
    std::fs::write(
        workspace.directory().join("outputs/libs/output.bin"),
        b"corrupt output",
    )
    .unwrap();
    assert_eq!(
        workspace
            .verify_after(budget.deadline, &SilentProgressReporter)
            .await
            .unwrap_err(),
        InstallError::ArtifactChanged
    );
}
#[tokio::test]
async fn rooted_runtime_changes_and_moved_job_directories_refuse_without_spawn() {
    let f = JobFixture::new();
    let budget = f.budget();
    let workspace = f.workspace(&budget).await;
    let runtime = f.installer.roots.base.join("runtime");
    std::fs::write(&runtime, b"inert").unwrap();
    assert_eq!(
        Invocation::fixture(
            &runtime,
            super::execution::args(workspace.cwd(), "job", &[]),
            Some(workspace),
            Pool::new()
        )
        .err()
        .unwrap(),
        InstallError::PathRefused
    );
    let workspace = f.workspace(&budget).await;
    let moved = f.installer.roots.base.join("moved");
    std::fs::rename(workspace.directory(), &moved).unwrap();
    assert_eq!(
        workspace
            .verify_inputs(budget.deadline, &SilentProgressReporter)
            .await
            .unwrap_err(),
        InstallError::PathRefused
    );
    assert!(moved.join("inputs/input.bin").exists());
    let workspace = f.workspace(&budget).await;
    let runtime = f.root.path().join("inert-runtime");
    std::fs::write(&runtime, b"inert").unwrap();
    let invocation = Invocation::fixture(&runtime, vec![], Some(workspace), Pool::new()).unwrap();
    std::fs::write(&runtime, b"changed").unwrap();
    let outcome = execution::run(invocation, budget, &SilentProgressReporter).await;
    assert_eq!(outcome.disposition, Disposition::NotStarted);
    assert_eq!(outcome.result.unwrap_err(), InstallError::ApprovalMismatch);
}
#[tokio::test]
async fn complete_inert_job_verifies_outputs_but_never_publishes_a_server_bundle() {
    let f = JobFixture::new();
    let budget = f.budget();
    let workspace = f.workspace(&budget).await;
    let directory = workspace.directory().to_path_buf();
    let pool = Pool::new();
    let outcome = execution::run(
        job(workspace, "job", pool.clone()),
        budget,
        &SilentProgressReporter,
    )
    .await;
    assert_eq!(outcome.disposition, Disposition::Exited);
    let output = outcome.result.unwrap();
    assert_eq!(
        output.workspace.as_ref().unwrap().verified().unwrap()[0].sha256,
        model::sha256(b"fixture output")
    );
    drop(output);
    assert_eq!(pool.available(), 2);
    assert!(directory.join("inputs/input.bin").exists());
    assert!(!f.installer.roots.base.join("instances").exists());
    assert!(!directory.join("control/receipt.json").exists());
    assert!(!directory.join("work/eula.txt").exists());
}
#[tokio::test]
async fn tampered_inputs_and_unexpected_outputs_fail_after_effects_without_cleanup_or_publication()
{
    for mode in ["job_tamper", "job_extra"] {
        let f = JobFixture::new();
        let budget = f.budget();
        let workspace = f.workspace(&budget).await;
        let directory = workspace.directory().to_path_buf();
        let outcome = execution::run(
            job(workspace, mode, Pool::new()),
            budget,
            &SilentProgressReporter,
        )
        .await;
        assert_eq!(outcome.disposition, Disposition::Exited);
        assert!(outcome.result.is_err());
        assert!(directory.join("outputs/libs/output.bin").exists());
        assert!(!f.installer.roots.base.join("instances").exists());
    }
}
#[tokio::test]
async fn uncertain_job_retains_workspace_installation_slot_and_volume_reservation() {
    let mut f = JobFixture::new();
    f.installer.fixture_space.as_mut().unwrap().1 = f.installer.limits.reserve_bytes + 27;
    let budget = f.budget();
    let workspace = f.workspace(&budget).await;
    let directory = workspace.directory().to_path_buf();
    let pool = Pool::new();
    let reporter = Arc::new(Reporter::default());
    let mut invocation = job(workspace, "hang", pool.clone());
    invocation.faults = Faults {
        unconfirmed_reap: true,
        refuse_handoff: true,
        ..Default::default()
    };
    #[cfg(windows)]
    let witness = super::execution::NativeWitness::attach(&mut invocation);
    let cloned = reporter.clone();
    let task = tokio::spawn(async move { execution::run(invocation, budget, &*cloned).await });
    super::execution::wait_for(|| directory.join("work/ready").exists()).await;
    reporter.cancel.store(true, Ordering::SeqCst);
    let outcome = task.await.unwrap();
    assert_eq!(outcome.disposition, Disposition::Unconfirmed);
    assert_eq!(outcome.result.unwrap_err(), InstallError::Cancelled);
    assert_eq!(pool.available(), 1);
    assert!(directory.join("inputs/input.bin").exists());
    let _other = f.installer.coordinator.slot().unwrap();
    assert_eq!(
        f.installer.coordinator.slot().err().unwrap(),
        InstallError::Busy
    );
    let (volume, free) = f.installer.space().unwrap();
    assert_eq!(
        f.installer
            .coordinator
            .reserve(volume, free, 1, f.installer.limits.reserve_bytes)
            .err()
            .unwrap(),
        InstallError::StorageLow
    );
    #[cfg(windows)]
    witness.assert_exited();
}
#[tokio::test]
async fn hard_linked_inputs_and_outputs_are_refused_without_reading_protected_aliases() {
    for tree in ["inputs", "outputs"] {
        let f = JobFixture::new();
        let budget = f.budget();
        let workspace = f.workspace(&budget).await;
        let (relative, body): (&str, &[u8]) = if tree == "inputs" {
            ("input.bin", b"fixture input")
        } else {
            ("libs/output.bin", b"fixture output")
        };
        let target = f.root.path().join("protected");
        std::fs::write(&target, body).unwrap();
        let alias = workspace.directory().join(tree).join(relative);
        if alias.exists() {
            std::fs::remove_file(&alias).unwrap();
        }
        std::fs::hard_link(&target, &alias).unwrap();
        let result = if tree == "inputs" {
            workspace
                .verify_inputs(budget.deadline, &SilentProgressReporter)
                .await
        } else {
            workspace
                .verify_after(budget.deadline, &SilentProgressReporter)
                .await
        };
        assert_eq!(result.unwrap_err(), InstallError::PathRefused);
        assert_eq!(std::fs::read(&target).unwrap(), body);
    }
}
#[cfg(windows)]
#[tokio::test]
async fn native_windows_junction_output_ancestor_refuses_and_preserves_target() {
    let f = JobFixture::new();
    let budget = f.budget();
    let workspace = f.workspace(&budget).await;
    let target = f.root.path().join("protected");
    std::fs::create_dir(&target).unwrap();
    std::fs::write(target.join("output.bin"), b"fixture output").unwrap();
    let junction = workspace.directory().join("outputs").join("libs");
    std::fs::remove_dir(&junction).unwrap();
    let result = std::process::Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(&junction)
        .arg(&target)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "owned junction fixture setup failed: stdout={:?}, stderr={:?}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(
        workspace
            .verify_after(budget.deadline, &SilentProgressReporter)
            .await
            .unwrap_err(),
        InstallError::PathRefused
    );
    assert_eq!(
        std::fs::read(target.join("output.bin")).unwrap(),
        b"fixture output"
    );
    std::fs::remove_dir(junction).unwrap();
}
#[cfg(unix)]
#[tokio::test]
async fn native_unix_linked_output_ancestor_refuses_and_preserves_target() {
    let f = JobFixture::new();
    let budget = f.budget();
    let workspace = f.workspace(&budget).await;
    let target = f.root.path().join("protected");
    std::fs::create_dir(&target).unwrap();
    std::fs::write(target.join("output.bin"), b"fixture output").unwrap();
    let link = workspace.directory().join("outputs/libs");
    std::fs::remove_dir(&link).unwrap();
    std::os::unix::fs::symlink(&target, &link).unwrap();
    assert_eq!(
        workspace
            .verify_after(budget.deadline, &SilentProgressReporter)
            .await
            .unwrap_err(),
        InstallError::PathRefused
    );
    assert_eq!(
        std::fs::read(target.join("output.bin")).unwrap(),
        b"fixture output"
    );
    std::fs::remove_file(link).unwrap();
}
#[tokio::test]
async fn overlapping_client_or_state_roots_cannot_construct_a_job() {
    let mut f = JobFixture::new();
    f.installer
        .roots
        .forbidden
        .push(f.installer.roots.base.clone());
    assert_eq!(
        workspace_result_error(
            Workspace::create(
                &f.installer,
                vec![],
                vec![file("output", b"x")],
                &f.budget(),
                &SilentProgressReporter
            )
            .await
        ),
        InstallError::PathRefused
    );
    assert!(entries(&f.installer.roots.base).is_empty());
}
#[tokio::test]
async fn unstarted_cleanup_never_sweeps_an_unrecognized_file() {
    let f = JobFixture::new();
    let budget = f.budget();
    let workspace = f.workspace(&budget).await;
    let directory = workspace.directory().to_path_buf();
    std::fs::write(directory.join("work/unrecognized"), b"retain").unwrap();
    drop(workspace);
    assert_eq!(
        std::fs::read(directory.join("work/unrecognized")).unwrap(),
        b"retain"
    );
    assert!(!directory.join("inputs/input.bin").exists());
    assert!(directory.exists());
}
