//! Retention through inherited pipes and Windows-only native unwind cleanup evidence.
mod common;
use common::*;
use std::{
    fs,
    path::PathBuf,
    thread,
    time::{Duration, Instant},
};
use vesta_game_runtime::process::{
    GracefulStop, LaunchSpec, ProcessPhase, RuntimeError, WaitOutcome,
};

struct ReleaseOnDrop(PathBuf);
impl Drop for ReleaseOnDrop {
    fn drop(&mut self) {
        let _ = fs::write(&self.0, b"release");
    }
}

#[test]
#[cfg_attr(unix, allow(unsafe_code))]
fn actual_child_exit_retains_draining_generation_until_inherited_pipes_really_close() {
    let mut fixture = Fixture::new(vec!["inherit-pipes".into()], GracefulStop::MinecraftCommand);
    // Declared after owner, so unwind publishes release BEFORE blocking owner destruction.
    let release = ReleaseOnDrop(fixture.dir.path().join("release-descendant"));
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let snapshot = fixture.process.poll().unwrap();
        if snapshot.phase == ProcessPhase::Draining
            && fixture.dir.path().join("descendant-ready").is_file()
        {
            assert_eq!(snapshot.generation, Some(fixture.generation));
            assert_eq!(snapshot.last_exit, None);
            break;
        }
        assert!(
            Instant::now() < deadline,
            "no actual child-exit/inherited-pipe witness"
        );
        thread::sleep(Duration::from_millis(5));
    }
    #[cfg(unix)]
    {
        let pid: libc::id_t = fs::read_to_string(fixture.dir.path().join("parent-pid"))
            .unwrap()
            .parse()
            .unwrap();
        // The runtime must not reap its Unix leader while a family kill is still
        // possible: retaining the zombie reserves the group ID against reuse.
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        assert_eq!(
            unsafe {
                libc::waitid(
                    libc::P_PID,
                    pid,
                    &mut info,
                    libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
                )
            },
            0
        );
        assert_eq!(unsafe { info.si_pid() } as libc::id_t, pid);
    }
    let replacement = LaunchSpec::new(
        program(),
        vec!["graceful".into()],
        fixture.dir.path().into(),
        GracefulStop::MinecraftCommand,
    )
    .unwrap();
    assert_eq!(
        fixture.process.start(replacement),
        Err(RuntimeError::AlreadyOwned)
    );
    let WaitOutcome::TimedOut(snapshot) = fixture
        .process
        .wait(fixture.generation, Duration::ZERO)
        .unwrap()
    else {
        panic!("held inherited pipes cannot be finalized");
    };
    assert_eq!(snapshot.phase, ProcessPhase::Draining);
    fs::write(&release.0, b"release").unwrap();
    let exit = fixture.stop();
    assert_eq!(exit.code, Some(0));
    assert!(!exit.output_failed);
    assert_eq!(fixture.recorded(0), Vec::<String>::new());
    assert_eq!(fixture.process.poll().unwrap().phase, ProcessPhase::Stopped);
    drop(release);
}

#[cfg(windows)]
#[test]
fn panic_unwind_terminates_live_direct_child_before_releasing_its_native_lease() {
    use std::{
        os::windows::fs::OpenOptionsExt,
        panic::{AssertUnwindSafe, catch_unwind},
    };
    let fixture = Fixture::new(vec!["closed-stdin".into()], GracefulStop::Unsupported);
    let lease = fixture.dir.path().join("lifetime-lease");
    let open_exclusive = || fs::OpenOptions::new().read(true).share_mode(0).open(&lease);
    assert!(
        open_exclusive().is_err(),
        "live fixture positively holds its exclusive native file lease"
    );
    let owner = fixture.process;
    let result = catch_unwind(AssertUnwindSafe(move || {
        let _owned = owner;
        panic!("controlled owner unwind");
    }));
    assert!(result.is_err());
    let _lease =
        open_exclusive().expect("owner unwind terminated child; its actual native lease released");
    assert!(
        fixture.dir.path().exists(),
        "working directory survives through native cleanup"
    );
}

#[test]
fn force_kill_terminates_inherited_pipe_helpers_in_running_and_draining_generations() {
    for mode in ["inherit-pipes", "inherit-pipes-running"] {
        for guarded in [false, true] {
            let mut fixture = Fixture::new(vec![mode.into()], GracefulStop::MinecraftCommand);
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                let state = fixture.process.poll().unwrap();
                if fixture.dir.path().join("descendant-ready").is_file()
                    && (mode != "inherit-pipes" || state.phase == ProcessPhase::Draining)
                {
                    break;
                }
                assert!(Instant::now() < deadline, "helper did not become ready");
                thread::sleep(Duration::from_millis(5));
            }
            let result = if guarded {
                fixture
                    .process
                    .force_kill_guarded(fixture.generation, Duration::from_secs(3), |_| {
                        Ok::<(), ()>(())
                    })
                    .unwrap()
            } else {
                fixture
                    .process
                    .force_kill(fixture.generation, Duration::from_secs(3))
                    .unwrap()
            };
            assert!(
                matches!(result, WaitOutcome::Exited(_)),
                "helper retained output after force kill: {result:?}"
            );
            assert_eq!(fixture.process.poll().unwrap().phase, ProcessPhase::Stopped);
            let next = LaunchSpec::new(
                program(),
                vec!["graceful".into()],
                fixture.dir.path().into(),
                GracefulStop::MinecraftCommand,
            )
            .unwrap();
            let generation = fixture.process.start(next).unwrap();
            assert!(matches!(
                fixture
                    .process
                    .stop(generation, Duration::from_secs(3))
                    .unwrap(),
                WaitOutcome::Exited(_)
            ));
        }
    }
}

#[test]
fn drop_after_direct_exit_also_terminates_inherited_pipe_helpers() {
    let mut fixture = Fixture::new(vec!["inherit-pipes".into()], GracefulStop::MinecraftCommand);
    let deadline = Instant::now() + Duration::from_secs(5);
    while fixture.process.poll().unwrap().phase != ProcessPhase::Draining
        || !fixture.dir.path().join("descendant-ready").is_file()
    {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(5));
    }
    let before = Instant::now();
    drop(fixture.process);
    assert!(
        before.elapsed() < Duration::from_secs(5),
        "drop waited for the helper's ten-second self-exit"
    );
}
