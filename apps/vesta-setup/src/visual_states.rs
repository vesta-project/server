//! Preview data passed through the exact production view constructors.
//! These values contain no real machine paths, keys, credentials, or identities.
use crate::{
    directory_picker::{self, Entry, Listing},
    engine::{self, Answers, Receipt, Startup},
    network::ExternalStatus,
    paths,
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
        external_probe: None,
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
        external_status: ExternalStatus::NotChecked,
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
    let root = paths::display(&fresh.root);
    let game = paths::display(existing.existing_server.as_ref().unwrap());
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
            directory_picker::view(
                &PathBuf::from(&game),
                &Listing {
                    entries: vec![
                        Entry {
                            path: PathBuf::from(&game).join("world"),
                            directory: true,
                        },
                        Entry {
                            path: PathBuf::from(&game).join("server.properties"),
                            directory: false,
                        },
                        Entry {
                            path: PathBuf::from(&game).join("readme.txt"),
                            directory: false,
                        },
                    ],
                    truncated: false,
                },
                0,
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
                if cfg!(windows) {
                    r"D:\Missing\minecraft▏"
                } else {
                    "/missing/minecraft▏"
                },
                Some("Choose an existing server directory.".into()),
            ),
        ),
        (
            "existing-not-detected",
            wizard::input_view(
                2,
                "Connect an existing server",
                "Existing Minecraft server folder",
                if cfg!(windows) {
                    r"C:\Users\Alex\Documents▏"
                } else {
                    "/home/alex/Documents▏"
                },
                Some("No Minecraft server files were detected in this directory.".into()),
            ),
        ),
        ("port-default", wizard::port_view(true, 0)),
        ("port-custom", wizard::port_view(true, 1)),
        ("port-busy", wizard::port_view(false, 1)),
        ("port-recheck", wizard::port_view(false, 2)),
        (
            "external-check",
            wizard::external_view("https://probe.example.org/v1/check", 0),
        ),
        (
            "external-skip",
            wizard::external_view("https://probe.example.org/v1/check", 1),
        ),
        ("checks", wizard::checks_view(&fresh)),
        ("existing-detected", wizard::checks_view(&existing)),
        (
            "port-conflict",
            wizard::input_view(
                3,
                "Choose a custom port",
                "Port (1–65535)",
                "8444▏",
                Some("That port is already in use. Choose another port or retry.".into()),
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
        (
            "external-reachable",
            external_result(&fresh, ExternalStatus::Reachable),
        ),
        (
            "external-unreachable",
            external_result(&fresh, ExternalStatus::Unreachable),
        ),
        (
            "external-unavailable",
            external_result(&fresh, ExternalStatus::Unavailable),
        ),
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
fn external_result(answers: &Answers, status: ExternalStatus) -> View {
    let mut result = receipt(answers.clone());
    result.external_status = status;
    engine::complete_view(&result)
}
