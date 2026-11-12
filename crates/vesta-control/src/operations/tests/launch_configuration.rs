//! Configured launch still uses real canonical admission and the owned driver.
use super::*;
use std::{
    fs,
    path::PathBuf,
    thread,
    time::{Duration, Instant},
};
use vesta_game_runtime::{launch_config::LaunchConfiguration, process::ProcessPhase};

const DEADLINE: Duration = Duration::from_secs(15);
const CHILD: &str = "operations::tests::launch_configuration::inert_configured_child";
const MARKER: &[u8] = b"owned vesta configuration test fixture";

#[test]
fn inert_configured_child() {
    if fs::read("configured-fixture").ok().as_deref() != Some(MARKER) {
        return;
    }
    fs::write("configured-ready", b"ready").unwrap();
    while !std::path::Path::new("release-configured-child").exists() {
        thread::sleep(Duration::from_millis(5));
    }
}

struct OwnedRuntime {
    runtime: OperationRuntime,
    release: PathBuf,
}
impl Drop for OwnedRuntime {
    fn drop(&mut self) {
        // Test-only cooperative release happens before observation-only runtime Drop.
        let _ = fs::write(&self.release, b"release");
    }
}

#[test]
fn trusted_launch_configuration_does_not_grant_authority_and_executes_only_admitted_start() {
    let f = Fixture::open();
    let cwd = f._root.path().join("configured game");
    fs::create_dir(&cwd).unwrap();
    fs::write(cwd.join("configured-fixture"), MARKER).unwrap();
    let text = format!(
        "[minecraft]\nworking-directory = '{}'\n[launch]\nkind = 'native'\nexecutable = '{}'\nstop = 'unsupported'\nargs = ['--exact', '{CHILD}', '--nocapture']",
        cwd.display(),
        std::env::current_exe().unwrap().display()
    );
    let configuration = LaunchConfiguration::parse_toml(text.as_bytes(), f._root.path()).unwrap();
    let launch = configuration.into_launch_spec(None).unwrap();
    let mut owned = OwnedRuntime {
        runtime: OperationRuntime::open(f.ops.clone(), launch).unwrap(),
        release: cwd.join("release-configured-child"),
    };
    assert_eq!(
        owned.runtime.game_snapshot().unwrap().phase,
        ProcessPhase::Stopped
    );
    assert!(!cwd.join("configured-ready").exists());
    let without_authority = seed_subject(&f.db, false);
    let refused = f
        .ops
        .submit_game(&without_authority, GameCommand::Start, None)
        .unwrap_err();
    assert_eq!(refused.code(), "forbidden");
    assert!(!cwd.join("configured-ready").exists());
    assert_eq!(f.counts().0, 0);
    let Admission::Created(start) = f
        .ops
        .submit_game(&f.subject, GameCommand::Start, None)
        .unwrap()
    else {
        panic!("fresh admission");
    };
    let until = Instant::now() + DEADLINE;
    loop {
        if cwd.join("configured-ready").is_file()
            && f.read(&start.id).state == OperationState::Succeeded
        {
            break;
        }
        assert!(
            Instant::now() < until,
            "configured admitted start did not complete"
        );
        thread::sleep(Duration::from_millis(5));
    }
    let snapshot = owned.runtime.game_snapshot().unwrap();
    assert_eq!(snapshot.phase, ProcessPhase::Running);
    assert!(snapshot.generation.is_some());
    assert!(f.held(&start.id).is_empty());
    assert_eq!(f.db.close().unwrap_err().code(), "state_runtime_busy");
    fs::write(&owned.release, b"release").unwrap();
    assert_eq!(
        owned.runtime.shutdown(DEADLINE).unwrap(),
        ShutdownOutcome::Complete
    );
    assert_eq!(
        owned.runtime.game_snapshot().unwrap().phase,
        ProcessPhase::Stopped
    );
    f.db.close().unwrap();
}
