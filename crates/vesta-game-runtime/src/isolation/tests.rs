use super::*;

struct Fixture {
    _root: tempfile::TempDir,
    game: PathBuf,
    runtime: PathBuf,
    protected: PathBuf,
    program: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let game = root.path().join("game");
        let runtime = root.path().join("jdk");
        let protected = root.path().join("private");
        for path in [&game, &runtime, &protected] {
            std::fs::create_dir(path).unwrap();
        }
        let program = runtime.join("java");
        std::fs::write(&program, b"inert-policy-fixture-not-an-executable").unwrap();
        Self {
            _root: root,
            game,
            runtime,
            protected,
            program,
        }
    }
    fn isolation(&self) -> ServerIsolation {
        ServerIsolation::new(vec![self.runtime.clone()], vec![self.protected.clone()]).unwrap()
    }
}

#[test]
fn server_policy_exposes_only_selected_runtime_game_and_exact_executable() {
    let f = Fixture::new();
    let (program, policy) = f.isolation().policy(&f.program, &f.game).unwrap();
    assert_eq!(policy.exec_allowlist, vec![program]);
    assert!(policy.enabled && policy.network_allowed);
    assert!(!policy.mic_allowed && !policy.usb_allowed);
    assert!(policy.extra_paths.is_empty());
    assert_eq!(policy.filesystem_allowlist.len(), 2);
    let game = &policy.filesystem_allowlist[0];
    assert!(game.read && game.write && game.load && !game.execute);
    let runtime = &policy.filesystem_allowlist[1];
    assert!(runtime.read && !runtime.write && runtime.load && !runtime.execute);
    assert!(!format!("{:?}", f.isolation()).contains(f.protected.to_str().unwrap()));
}

#[test]
fn protected_paths_cannot_be_exposed_by_game_runtime_or_parent_roots_even_when_absent() {
    let f = Fixture::new();
    for protected in [
        f.game.join("not-yet-created-key.pem"),
        f.runtime.join("state"),
        f._root.path().into(),
    ] {
        let policy = ServerIsolation::new(vec![f.runtime.clone()], vec![protected]).unwrap();
        assert!(matches!(
            policy.policy(&f.program, &f.game),
            Err(RuntimeError::IsolationUnavailable)
        ));
    }
    let policy =
        ServerIsolation::new(vec![f._root.path().into()], vec![f.protected.clone()]).unwrap();
    assert!(policy.policy(&f.program, &f.game).is_err());
}

#[test]
fn runtime_membership_and_current_path_shape_are_required() {
    let f = Fixture::new();
    assert!(
        f.isolation()
            .policy(&std::env::current_exe().unwrap(), &f.game)
            .is_err()
    );
    std::fs::remove_dir(&f.game).unwrap();
    std::fs::write(&f.game, b"replacement").unwrap();
    assert!(f.isolation().policy(&f.program, &f.game).is_err());
    assert!(ServerIsolation::new(vec!["relative".into()], vec![f.protected.clone()]).is_err());
    assert!(ServerIsolation::new(vec![f.runtime.clone()], vec![]).is_err());
}

#[test]
fn canonical_retarget_to_protected_directory_refuses() {
    let f = Fixture::new();
    std::fs::remove_dir(&f.game).unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(&f.protected, &f.game).unwrap();
    #[cfg(windows)]
    std::os::windows::fs::symlink_dir(&f.protected, &f.game).unwrap();
    assert!(f.isolation().policy(&f.program, &f.game).is_err());
}

#[test]
fn cleanup_is_lifetime_owned_and_does_not_follow_an_inside_link() {
    let root = tempfile::tempdir().unwrap();
    let outside = root.path().join("witness");
    std::fs::create_dir(&outside).unwrap();
    std::fs::write(outside.join("keep"), b"unchanged").unwrap();
    let private = root.path().join("private-temp");
    std::fs::create_dir(&private).unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(&outside, private.join("link")).unwrap();
    #[cfg(windows)]
    std::os::windows::fs::symlink_dir(&outside, private.join("link")).unwrap();
    let lifetime = IsolationLifetime {
        paths: vec![private.clone()],
    };
    assert!(private.exists());
    drop(lifetime);
    assert!(!private.exists());
    assert_eq!(std::fs::read(outside.join("keep")).unwrap(), b"unchanged");
}

#[test]
fn software_replacement_cannot_drop_policy_or_change_to_unapproved_runtime() {
    use crate::process::{GracefulStop, LaunchSpec};
    let f = Fixture::new();
    let make = |program| {
        LaunchSpec::new(
            program,
            vec![],
            f.game.clone(),
            GracefulStop::MinecraftCommand,
        )
        .unwrap()
    };
    let old = make(f.program.clone())
        .with_isolation(f.isolation())
        .unwrap();
    let replacement = old.constrain_replacement(make(f.program.clone())).unwrap();
    assert!(replacement.is_isolated());
    assert!(
        old.constrain_replacement(make(std::env::current_exe().unwrap()))
            .is_err()
    );
    assert!(
        !make(f.program.clone())
            .constrain_replacement(make(f.program.clone()))
            .unwrap()
            .is_isolated()
    );
}
