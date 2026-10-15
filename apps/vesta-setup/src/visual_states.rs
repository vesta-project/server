//! Preview data passed through the exact production view constructors.
//! These values contain no real machine paths, keys, credentials, or identities.
use crate::{
    engine::{self, Answers, Receipt, Startup},
    ui::View,
    wizard,
};
use std::path::PathBuf;

fn answers() -> Answers {
    Answers {
        schema: 1,
        root: PathBuf::from(if cfg!(windows) {
            r"C:\Vesta\my-server"
        } else {
            "/home/alex/.local/share/Vesta/my-server"
        }),
        existing_server: None,
        startup: Startup::Automatic,
        port: 8443,
    }
}
fn receipt(answers: Answers) -> Receipt {
    Receipt {
        schema: 1,
        installation_id: "0123456789abcdef0123456789abcdef".into(),
        answers,
        release_version: "0.1.0".into(),
        release_manifest_sha256: "a".repeat(64),
        phase: "ready".into(),
        tls_sha256: "8a2c714f9d33ae068155c47a20bef4bfa7ba65d4bd369c88dc404ffd923402e7".into(),
        startup_registered: true,
    }
}
pub fn all() -> Vec<(&'static str, View)> {
    let fresh = answers();
    let mut existing = fresh.clone();
    existing.existing_server = Some(PathBuf::from(if cfg!(windows) {
        r"D:\Minecraft\survival"
    } else {
        "/home/alex/minecraft/survival"
    }));
    let mut manual = fresh.clone();
    manual.startup = Startup::Manual;
    let root = fresh.root.display().to_string();
    let game = existing
        .existing_server
        .as_ref()
        .unwrap()
        .display()
        .to_string();
    let location = || {
        wizard::input_view(
            2,
            "Where should Vesta live?",
            "Dedicated Vesta installation folder",
            &format!("{root}▏"),
            None,
        )
    };
    vec![
        ("welcome", wizard::welcome_view(0)),
        ("welcome-existing", wizard::welcome_view(1)),
        ("location-new", location()),
        (
            "location-existing",
            wizard::input_view(
                2,
                "Connect an existing server",
                "Existing Minecraft server folder",
                &format!("{game}▏"),
                None,
            ),
        ),
        (
            "location-invalid",
            wizard::input_view(
                2,
                "Where should Vesta live?",
                "Dedicated Vesta installation folder",
                &format!("{game}▏"),
                Some(
                    "Keep Vesta's installation folder separate from the existing Minecraft server."
                        .into(),
                ),
            ),
        ),
        (
            "existing-unavailable",
            wizard::input_view(
                2,
                "Connect an existing server",
                "Existing Minecraft server folder",
                "/missing/minecraft▏",
                Some("Choose an existing server directory.".into()),
            ),
        ),
        (
            "existing-not-detected",
            wizard::input_view(
                2,
                "Connect an existing server",
                "Existing Minecraft server folder",
                "/home/alex/Documents▏",
                Some("No Minecraft server files were detected in this directory.".into()),
            ),
        ),
        ("checks", wizard::checks_view(&fresh)),
        ("existing-detected", wizard::checks_view(&existing)),
        (
            "port-conflict",
            wizard::input_view(
                3,
                "Choose an available local port",
                "The usual port is busy. Use another port for this host.",
                "8444▏",
                Some("That port is unavailable. Choose another port.".into()),
            ),
        ),
        ("startup", wizard::startup_view(true, 0)),
        ("startup-manual", wizard::startup_view(true, 1)),
        ("startup-unavailable", wizard::startup_view(false, 1)),
        ("review", wizard::review_view(&fresh, 0)),
        ("review-existing", wizard::review_view(&existing, 0)),
        ("review-back", wizard::review_view(&manual, 1)),
        (
            "progress-preparing",
            engine::progress_view("preparing", "Creating private host storage", 0, 0),
        ),
        (
            "progress-installing",
            engine::progress_view(
                "installing",
                if cfg!(windows) {
                    "vesta-agent.exe"
                } else {
                    "vesta-agent"
                },
                28 * 1048576,
                48 * 1048576,
            ),
        ),
        (
            "progress-startup",
            engine::progress_view("startup", "Starting the host", 0, 0),
        ),
        (
            "progress-verifying",
            engine::progress_view("verifying", "Checking the secure local connection", 0, 0),
        ),
        ("complete", engine::complete_view(&receipt(fresh.clone()))),
        ("complete-manual", engine::complete_view(&receipt(manual))),
        ("failure", engine::failure_view("setup_write_failed")),
        (
            "failure-signature",
            engine::failure_view("release signature verification failed"),
        ),
        (
            "failure-startup",
            engine::failure_view("setup_host_start_failed"),
        ),
        (
            "failure-space",
            engine::failure_view("setup_disk_space_low"),
        ),
        ("cancelled", engine::cancelled_view()),
        ("resume", wizard::resume_view(&fresh, 0)),
        ("resume-another", wizard::resume_view(&fresh, 1)),
        ("narrow", location()),
        ("plain", engine::complete_view(&receipt(fresh))),
    ]
}
