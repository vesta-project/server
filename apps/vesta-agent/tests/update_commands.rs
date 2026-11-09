//! Both public names dispatch the same OS-local update code, with no new shell,
//! network source, activation authority or guessed installation root.
use std::process::Command;

#[test]
fn public_update_commands_share_help_and_reject_unsafe_or_missing_arguments() {
    let root = tempfile::tempdir().unwrap();
    for binary in [
        env!("CARGO_BIN_EXE_vesta"),
        env!("CARGO_BIN_EXE_vesta-agent"),
    ] {
        let help = Command::new(binary)
            .args(["update", "--help"])
            .current_dir(root.path())
            .output()
            .unwrap();
        assert!(help.status.success());
        let help = String::from_utf8(help.stdout).unwrap();
        assert!(help.contains("configure"));
        assert!(help.contains("--startup-check"));
        assert!(help.contains("never activate or restart"));
        for args in [
            vec!["update", "apply"],
            vec!["update", "check"],
            vec!["update", "stage", "--force"],
            vec!["update", "verify", "--manifest-sha256", "../escape"],
        ] {
            let result = Command::new(binary)
                .args(args)
                .current_dir(root.path())
                .output()
                .unwrap();
            assert!(!result.status.success());
        }
        let bare = Command::new(binary)
            .arg("update")
            .current_dir(root.path())
            .output()
            .unwrap();
        assert!(!bare.status.success());
        assert!(
            String::from_utf8_lossy(&bare.stderr)
                .contains("self_update_activation_not_implemented")
        );
    }
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
}
