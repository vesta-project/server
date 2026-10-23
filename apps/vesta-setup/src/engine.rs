//! Journalled host preparation. Existing Minecraft files are only observed.
use crate::{
    network::{self, ExternalProbe, ExternalStatus},
    paths, platform,
    release::Release,
    ui::{Row, TaskStatus, Tone, View},
};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    net::{Ipv4Addr, TcpListener},
    path::{Component, Path, PathBuf},
};
use vesta_state::StateDirectory;

pub type Result<T> = std::result::Result<T, String>;
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Startup {
    Automatic,
    Manual,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Answers {
    pub schema: u8,
    pub root: PathBuf,
    pub existing_server: Option<PathBuf>,
    pub startup: Startup,
    #[serde(default = "network::default_port")]
    pub port: u16,
    #[serde(default)]
    pub external_probe: Option<ExternalProbe>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Receipt {
    pub schema: u8,
    pub installation_id: String,
    pub answers: Answers,
    pub release_version: String,
    pub release_manifest_sha256: String,
    pub phase: String,
    pub tls_sha256: String,
    pub startup_registered: bool,
    #[serde(default)]
    pub external_status: ExternalStatus,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Handoff {
    pub schema: u8,
    pub installation_id: String,
    pub endpoint: String,
    pub tls_sha256: String,
    pub status: String,
    pub app_uri: String,
}
pub fn default_root() -> PathBuf {
    #[cfg(windows)]
    let base = std::env::var_os("LOCALAPPDATA").map(PathBuf::from);
    #[cfg(not(windows))]
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local").join("share"))
        });
    paths::normalize(
        &base
            .unwrap_or_else(|| PathBuf::from("/var/empty"))
            .join("Vesta")
            .join("hosts")
            .join("default"),
    )
}
pub fn guard(path: &Path) -> Result<()> {
    if !path.is_absolute()
        || path
            .to_str()
            .is_none_or(|s| s.chars().any(char::is_control))
        || path
            .components()
            .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
    {
        return Err("setup_path_invalid".into());
    }
    for part in path.ancestors() {
        match fs::symlink_metadata(part) {
            Ok(meta) => {
                #[cfg(windows)]
                {
                    use std::os::windows::fs::MetadataExt;
                    if meta.file_attributes() & 0x400 != 0 {
                        return Err("setup_link_refused".into());
                    }
                }
                if meta.file_type().is_symlink() {
                    return Err("setup_link_refused".into());
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err("setup_path_unavailable".into()),
        }
    }
    Ok(())
}
pub fn read_private(path: &Path, limit: u64) -> Result<Vec<u8>> {
    guard(path)?;
    let m = fs::symlink_metadata(path).map_err(|_| "setup_file_unavailable")?;
    if !m.is_file() || m.len() > limit {
        return Err("setup_file_invalid".into());
    }
    let mut bytes = Vec::new();
    File::open(path)
        .map_err(|_| "setup_file_unavailable")?
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "setup_file_unavailable")?;
    if bytes.len() as u64 > limit {
        return Err("setup_file_too_large".into());
    }
    Ok(bytes)
}
pub fn read_answers(path: &Path) -> Result<Answers> {
    let path = if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir()
            .map_err(|_| "setup_directory_unavailable")?
            .join(path)
    };
    let mut answers: Answers = serde_json::from_slice(&read_private(&path, 16384)?)
        .map_err(|_| "setup_answers_invalid")?;
    validate_answers(&answers)?;
    answers.root = paths::normalize(&answers.root);
    answers.existing_server = answers.existing_server.as_deref().map(paths::normalize);
    Ok(answers)
}
fn comparable(path: &Path) -> String {
    paths::comparable(path)
}
fn overlaps(left: &Path, right: &Path) -> bool {
    let a = comparable(left).trim_end_matches('/').to_owned();
    let b = comparable(right).trim_end_matches('/').to_owned();
    a == b || a.starts_with(&(b.clone() + "/")) || b.starts_with(&(a + "/"))
}
pub fn validate_answers(answers: &Answers) -> Result<()> {
    if answers.schema != 1 || answers.port == 0 {
        return Err("setup_answers_invalid".into());
    }
    platform::target()?;
    if let Some(probe) = &answers.external_probe {
        probe.validate()?;
    }
    guard(&answers.root)?;
    if answers.root.parent().is_none()
        || answers.root.parent().is_some_and(|p| p.parent().is_none())
    {
        return Err("setup_choose_dedicated_directory".into());
    }
    if let Some(game) = &answers.existing_server {
        guard(game)?;
        if !game.is_dir() {
            return Err("setup_existing_server_missing".into());
        }
        if overlaps(game, &answers.root) {
            return Err("setup_existing_server_overlap".into());
        }
        if !["server.properties", "eula.txt", "world", "mods", "plugins"]
            .iter()
            .any(|name| game.join(name).exists())
        {
            return Err("setup_existing_server_not_detected".into());
        }
    }
    if answers.root.exists() {
        if !answers.root.is_dir() {
            return Err("setup_choose_dedicated_directory".into());
        }
        let contents = fs::read_dir(&answers.root)
            .map_err(|_| "setup_path_unavailable")?
            .next();
        if contents.is_some() && !answers.root.join("private").join("setup.json").is_file() {
            return Err("setup_directory_not_empty".into());
        }
    }
    Ok(())
}
pub fn port_available(port: u16) -> bool {
    TcpListener::bind((Ipv4Addr::LOCALHOST, port)).is_ok()
}
pub fn read_receipt(root: &Path) -> Result<Receipt> {
    guard(root)?;
    let r: Receipt = serde_json::from_slice(&read_private(
        &root.join("private").join("setup.json"),
        32768,
    )?)
    .map_err(|_| "setup_receipt_invalid")?;
    if r.schema != 1
        || comparable(&r.answers.root) != comparable(root)
        || r.installation_id.len() != 32
        || !r
            .installation_id
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    {
        return Err("setup_receipt_invalid".into());
    }
    Ok(r)
}
fn private_directory(root: &Path) -> Result<StateDirectory> {
    let dir = StateDirectory::new(root.join("private"), None)
        .map_err(|_| "setup_private_directory_refused")?;
    dir.prepare()
        .map_err(|_| "setup_private_directory_refused")?;
    Ok(dir)
}
pub fn write_private(root: &Path, name: &str, bytes: &[u8]) -> Result<()> {
    if ![
        "setup.json",
        "handoff.json",
        "certificate.pem",
        "key.pem",
        "stop-request",
    ]
    .contains(&name)
    {
        return Err("setup_file_invalid".into());
    }
    let directory = private_directory(root)?;
    let target = root.join("private").join(name);
    guard(&target)?;
    let temp = root
        .join("private")
        .join(format!(".setup-{}", uuid::Uuid::new_v4().simple()));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let result = (|| {
        let mut file = options.open(&temp).map_err(|_| "setup_write_failed")?;
        directory
            .tighten_file(&temp)
            .map_err(|_| "setup_private_permissions_failed")?;
        file.write_all(bytes)
            .and_then(|_| file.sync_all())
            .map_err(|_| "setup_write_failed")?;
        drop(file);
        guard(&target)?;
        fs::rename(&temp, &target).map_err(|_| "setup_write_failed")?;
        #[cfg(unix)]
        File::open(root.join("private"))
            .and_then(|directory| directory.sync_all())
            .map_err(|_| "setup_write_failed")?;
        directory
            .tighten_file(&target)
            .map_err(|_| "setup_private_permissions_failed")?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(temp);
    }
    result
}
fn journal(root: &Path, receipt: &Receipt) -> Result<()> {
    write_private(
        root,
        "setup.json",
        &serde_json::to_vec_pretty(receipt).map_err(|_| "setup_write_failed")?,
    )
}
pub fn handoff(receipt: &Receipt) -> Handoff {
    let endpoint = format!("https://127.0.0.1:{}", receipt.answers.port);
    let uri = format!(
        "vesta://setup?endpoint={}&tls_sha256={}&installation_id={}",
        url::form_urlencoded::byte_serialize(endpoint.as_bytes()).collect::<String>(),
        receipt.tls_sha256,
        receipt.installation_id
    );
    Handoff {
        schema: 1,
        installation_id: receipt.installation_id.clone(),
        endpoint,
        tls_sha256: receipt.tls_sha256.clone(),
        status: "awaiting_app_onboarding".into(),
        app_uri: uri,
    }
}
pub fn prepare(
    answers: &Answers,
    release: &Release,
    progress: &mut dyn FnMut(&str, &str, u64, u64),
) -> Result<Receipt> {
    guard(&answers.root.join("private/uninstall-pending"))?;
    if answers.root.join("private/uninstall-pending").exists() {
        return Err("setup_uninstall_pending".into());
    }
    release.ensure_intact()?;
    validate_answers(answers)?;
    let previous = if answers.root.join("private").join("setup.json").exists() {
        Some(read_receipt(&answers.root)?)
    } else {
        None
    };
    if previous.as_ref().is_some_and(|r| r.phase == "activated") {
        return Err("setup_already_activated_use_app".into());
    }
    if previous.as_ref().is_some_and(|r| {
        r.answers != *answers || r.release_manifest_sha256 != release.manifest_hash()
    }) {
        return Err("setup_resume_settings_mismatch".into());
    }
    if !port_available(answers.port) && !previous.as_ref().is_some_and(crate::host::ready) {
        return Err("setup_port_in_use".into());
    }
    let required = release.manifest.files.iter().map(|f| f.size).sum::<u64>() + 256 * 1024 * 1024;
    let ancestor = answers
        .root
        .ancestors()
        .find(|p| p.is_dir())
        .ok_or("setup_path_unavailable")?;
    if fs2::available_space(ancestor).map_err(|_| "setup_disk_space_unknown")? < required {
        return Err("setup_disk_space_low".into());
    }
    fs::create_dir_all(&answers.root).map_err(|_| "setup_directory_create_failed")?;
    StateDirectory::new(&answers.root, None)
        .and_then(|dir| dir.prepare())
        .map_err(|_| "setup_private_directory_refused")?;
    let directory = private_directory(&answers.root)?;
    let lock_path = answers.root.join("private").join("setup.lock");
    guard(&lock_path)?;
    let lock = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
        .map_err(|_| "setup_lock_failed")?;
    directory
        .tighten_file(&lock_path)
        .map_err(|_| "setup_private_permissions_failed")?;
    lock.try_lock_exclusive()
        .map_err(|_| "setup_already_running")?;
    if answers.root.join("private/uninstall-pending").exists() {
        return Err("setup_uninstall_pending".into());
    }
    let receipt_path = answers.root.join("private").join("setup.json");
    let mut receipt = if receipt_path.exists() {
        let r = read_receipt(&answers.root)?;
        if r.answers != *answers || r.release_manifest_sha256 != release.manifest_hash() {
            return Err("setup_resume_settings_mismatch".into());
        }
        r
    } else {
        Receipt {
            schema: 1,
            installation_id: uuid::Uuid::new_v4().simple().to_string(),
            answers: answers.clone(),
            release_version: release.manifest.version.clone(),
            release_manifest_sha256: release.manifest_hash().into(),
            phase: "preparing".into(),
            tls_sha256: String::new(),
            startup_registered: false,
            external_status: ExternalStatus::NotChecked,
        }
    };
    if !port_available(answers.port) && !crate::host::ready(&receipt) {
        return Err("setup_port_in_use".into());
    }
    journal(&answers.root, &receipt)?;
    progress("preparing", "Creating private host storage", 0, 0);
    progress("preparing", "Creating the local TLS identity", 0, 0);
    if receipt.tls_sha256.is_empty() {
        let has_key = answers.root.join("private").join("key.pem").exists();
        let has_cert = answers
            .root
            .join("private")
            .join("certificate.pem")
            .exists();
        if has_cert && !has_key {
            return Err("setup_tls_partial_recovery_required".into());
        }
        if !has_cert {
            let key = if has_key {
                let bytes = read_private(&answers.root.join("private").join("key.pem"), 16384)?;
                rcgen::KeyPair::from_pem(
                    std::str::from_utf8(&bytes).map_err(|_| "setup_tls_invalid")?,
                )
                .map_err(|_| "setup_tls_invalid")?
            } else {
                rcgen::KeyPair::generate().map_err(|_| "setup_tls_generation_failed")?
            };
            let cert = rcgen::CertificateParams::new(vec!["localhost".into(), "127.0.0.1".into()])
                .and_then(|params| params.self_signed(&key))
                .map_err(|_| "setup_tls_generation_failed")?;
            if !has_key {
                write_private(&answers.root, "key.pem", key.serialize_pem().as_bytes())?;
            }
            write_private(&answers.root, "certificate.pem", cert.pem().as_bytes())?;
        }
        // Recover credentials from an interrupted first install without rotating them.
        receipt.tls_sha256 = crate::host::identity_fingerprint(&answers.root)?;
        journal(&answers.root, &receipt)?;
    }
    release.install(&answers.root, &mut |name, done, total| {
        progress("installing", name, done, total)
    })?;
    receipt.phase = "installed".into();
    journal(&answers.root, &receipt)?;
    let descriptor = handoff(&receipt);
    write_private(
        &answers.root,
        "handoff.json",
        &serde_json::to_vec_pretty(&descriptor).map_err(|_| "setup_write_failed")?,
    )?;
    let executable = answers.root.join("bin").join(if cfg!(windows) {
        "vesta-setup.exe"
    } else {
        "vesta-setup"
    });
    progress("startup", "Starting the host", 0, 0);
    if !crate::host::ready(&receipt) {
        // A previous explicit local stop is not a permanent startup disablement.
        let stop = answers.root.join("private").join("stop-request");
        guard(&stop)?;
        if stop.exists() {
            fs::remove_file(&stop).map_err(|_| "setup_restart_failed")?;
        }
        if answers.startup == Startup::Automatic {
            platform::enable_startup(&answers.root, &executable, &receipt.installation_id)?;
            receipt.startup_registered = true;
            journal(&answers.root, &receipt)?;
        } else {
            platform::start_host(&answers.root, &executable)?;
        }
    }
    progress("verifying", "Checking the secure local connection", 0, 0);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(12);
    while !crate::host::ready(&receipt) {
        if std::time::Instant::now() >= deadline {
            return Err("setup_host_start_failed".into());
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    receipt.phase = "ready".into();
    if answers.external_probe.is_some() {
        progress("verifying", "Checking external access", 0, 0);
    }
    receipt.external_status = network::diagnose(&receipt, &network::HttpsProbe);
    journal(&answers.root, &receipt)?;
    Ok(receipt)
}
pub fn progress_view(phase: &str, file: &str, done: u64, total: u64) -> View {
    let stages = [
        ("preparing", "Prepare private storage"),
        ("installing", "Install verified Vesta components"),
        ("startup", "Set up host startup"),
        ("verifying", "Verify the local connection"),
    ];
    let current = stages.iter().position(|(id, _)| *id == phase).unwrap_or(0);
    let mut rows: Vec<_> = stages
        .iter()
        .enumerate()
        .map(|(i, (_, label))| Row::Task {
            label: (*label).into(),
            status: if i < current {
                TaskStatus::Done
            } else if i == current {
                TaskStatus::Active
            } else {
                TaskStatus::Pending
            },
        })
        .collect();
    rows.push(Row::Empty);
    rows.push(Row::Detail {
        label: "Now".into(),
        value: if total > 0 {
            format!("{file} · {} / {} MiB", done / 1048576, total / 1048576)
        } else {
            file.into()
        },
    });
    View {
        step: 6,
        heading: "Preparing your host".into(),
        description: vec![],
        rows,
        hint: "Interrupted setup can be resumed with the same options.".into(),
    }
}
pub fn complete_view(receipt: &Receipt) -> View {
    let h = handoff(receipt);
    View {
        step: 6,
        heading: "Your host is ready".into(),
        description: vec!["Vesta is installed and the local connection is open.".into()],
        rows: vec![
            Row::Detail {
                label: "External access".into(),
                value: receipt.external_status.label().into(),
            },
            Row::Detail {
                label: "Connection".into(),
                value: h.endpoint,
            },
            Row::Detail {
                label: "Startup".into(),
                value: if receipt.answers.startup == Startup::Automatic {
                    platform::startup_description().into()
                } else {
                    "Manual · running for this session".into()
                },
            },
            Row::Detail {
                label: "Installed in".into(),
                value: paths::display(&receipt.answers.root),
            },
        ],
        hint: String::new(),
    }
}
pub fn failure_view(code: &str) -> View {
    let message = match code {
        "setup_write_failed" | "setup_create_failed" | "setup_private_permissions_failed" => {
            "Vesta could not save its installation files. Check that you can write to the selected folder."
        }
        "setup_disk_space_low" => {
            "There is not enough free disk space. Free some space or choose another installation drive."
        }
        "setup_host_start_failed" => {
            "The local host did not become ready. Check for another application using its port, then try again."
        }
        "setup_port_in_use" => {
            "Another application is using the selected port. Choose a free local port and try again."
        }
        "setup_resume_settings_mismatch" => {
            "This folder belongs to a previous setup. Resume with its original options or choose a new folder."
        }
        "setup_tls_partial_recovery_required" | "setup_tls_identity_changed" => {
            "The saved connection identity needs recovery. Keep this installation folder and its private files."
        }
        "setup_already_running" => {
            "Another setup is using this folder. Let it finish before trying again."
        }
        "release signature verification failed"
        | "manifest hash mismatch"
        | "artifact hash mismatch" => {
            "The release could not be verified. Run the published setup command again using its original verification values."
        }
        _ => code,
    };
    View{step:6,heading:"Setup needs your attention".into(),description:vec!["Setup stopped before declaring the host ready.".into()],rows:vec![Row::Notice{tone:Tone::Error,text:message.into()},Row::Notice{tone:Tone::Info,text:"Your existing Minecraft files are preserved. Correct the issue and rerun the same setup command.".into()}],hint:"If setup was interrupted, keep its installation folder for recovery.".into()}
}
pub fn cancelled_view() -> View {
    View {
        step: 6,
        heading: "Setup cancelled".into(),
        description: vec!["No installation was started.".into()],
        rows: vec![],
        hint: "Run the setup command again whenever you are ready.".into(),
    }
}
pub fn export_visuals(path: &Path) -> Result<()> {
    guard(path)?;
    fs::create_dir(path).map_err(|_| "setup_preview_directory_must_be_new")?;
    let mut index = Vec::new();
    for (name, view) in crate::visual_states::all() {
        let theme = crate::ui::Theme {
            color: name != "plain",
            unicode: name != "plain",
            width: if name == "narrow" { 44 } else { 88 },
        };
        let ansi = crate::ui::render(&view, theme);
        fs::write(path.join(format!("{name}.ansi")), ansi.as_bytes())
            .map_err(|_| "setup_preview_write_failed")?;
        index.push(serde_json::json!({"id":name,"title":view.heading,"ansi":ansi,"columns":if name=="narrow"{44}else{88}}));
    }
    fs::write(
        path.join("states.json"),
        serde_json::to_vec_pretty(&index).map_err(|_| "setup_preview_write_failed")?,
    )
    .map_err(|_| "setup_preview_write_failed")?;
    Ok(())
}
