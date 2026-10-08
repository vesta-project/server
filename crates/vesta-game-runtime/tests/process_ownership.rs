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
