//! Linux system installation reuses the signed release loader; development input is explicit.
#[cfg(target_os = "linux")]
pub fn run(args: &[String]) -> Result<(), String> {
    use std::{collections::BTreeMap, path::PathBuf, process::Command};
    let mut values = BTreeMap::new();
    let mut forwarded = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let key = args[i].as_str();
        match key {
            "--local-only" | "--no-start" | "--no-service" | "--yes" => {
                forwarded.push(args[i].clone())
            }
            "--root" | "--port" | "--server-dir" | "--answers" | "--from-dir" | "--tag"
            | "--channel" | "--bundle-url" | "--bundle-dir" | "--manifest-sha256"
            | "--release-key" | "--asset-prefix" => {
                i += 1;
                let value = args
                    .get(i)
                    .filter(|v| !v.starts_with("--"))
                    .ok_or("Missing option value")?;
                if values.insert(key, value.clone()).is_some() {
                    return Err("Duplicate option".into());
                }
                if matches!(key, "--root" | "--port" | "--server-dir" | "--answers") {
                    forwarded.extend([key.to_owned(), value.clone()]);
                }
            }
            "--no-color" | "--ascii" => (),
            _ => return Err(format!("Unknown Linux installation option: {key}")),
        }
        i += 1;
    }
    require_root()?;
    let stage = std::env::temp_dir().join(format!("vesta-linux-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&stage).map_err(|e| e.to_string())?;
    let result = (|| {
        let binaries = if let Some(dir) = values.get("--from-dir") {
            eprintln!("WARNING: UNVERIFIED DEVELOPMENT installation from local binaries.");
            PathBuf::from(dir)
                .canonicalize()
                .map_err(|e| e.to_string())?
        } else {
            let release = if values.contains_key("--bundle-url")
                || values.contains_key("--bundle-dir")
            {
                crate::release::load(crate::release::Source {
                    local: values.get("--bundle-dir").map(PathBuf::from),
                    base_url: values.get("--bundle-url").cloned(),
                    manifest_sha256: values
                        .get("--manifest-sha256")
                        .cloned()
                        .ok_or("Manifest pin required")?,
                    release_key: values
                        .get("--release-key")
                        .cloned()
                        .ok_or("Release key required")?,
                    asset_prefix: values.get("--asset-prefix").cloned(),
                })?
            } else {
                if values.contains_key("--tag") && values.contains_key("--channel") {
                    return Err("Choose tag or channel".into());
                }
                let tag = match values.get("--tag") {
                    Some(t) => t.clone(),
                    None => {
                        let channel = values
                            .get("--channel")
                            .map(String::as_str)
                            .unwrap_or("nightly");
                        if !matches!(channel, "stable" | "nightly") {
                            return Err("Invalid channel".into());
                        }
                        let entries = crate::release::official::catalog()?;
                        crate::release::official::latest(&entries, channel == "nightly")?.to_owned()
                    }
                };
                crate::release::official::load(
                    &tag,
                    include_str!("../../../scripts/bootstrap/release-public-key.txt").trim(),
                )?
            };
            release.install(&stage, &mut |_, _, _| {})?;
            stage.join("bin")
        };
        let status = Command::new("bash")
            .arg("-c")
            .arg(SCRIPT)
            .arg("vesta-linux-install")
            .arg("install")
            .arg(binaries)
            .args(forwarded)
            .status()
            .map_err(|e| e.to_string())?;
        if status.success() {
            Ok(())
        } else {
            Err("Linux installation incomplete; correct the reported error and rerun the same command".into())
        }
    })();
    let _ = std::fs::remove_dir_all(stage);
    result
}
#[cfg(target_os = "linux")]
fn require_root() -> Result<(), String> {
    let output = std::process::Command::new("id")
        .arg("-u")
        .output()
        .map_err(|e| e.to_string())?;
    if output.stdout != b"0\n" {
        return Err("Installing Vesta Server requires root. Run the command with sudo (sudo bash install.sh ...).".into());
    }
    Ok(())
}
#[cfg(target_os = "linux")]
pub fn admin(args: &[std::ffi::OsString]) -> std::process::ExitCode {
    if let Err(e) = require_root() {
        eprintln!("{e}");
        return std::process::ExitCode::FAILURE;
    }
    match std::process::Command::new("bash")
        .arg("-c")
        .arg(SCRIPT)
        .arg("vesta-linux-admin")
        .args(args)
        .status()
    {
        Ok(s) => std::process::ExitCode::from(s.code().unwrap_or(1) as u8),
        Err(e) => {
            eprintln!("{e}");
            std::process::ExitCode::FAILURE
        }
    }
}
#[cfg(target_os = "linux")]
const SCRIPT: &str = include_str!("linux-service.sh");
/// Written by the system installer; its presence marks a system installation.
pub const DATA_ROOT_RECORD: &str = "/usr/local/lib/vesta/data-root";

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    #[test]
    fn linux_unit_is_system_hardened_and_runs_host() {
        let o = std::process::Command::new("bash")
            .args(["-c", SCRIPT, "test", "print-unit", "/var/lib/vesta"])
            .output()
            .unwrap();
        assert!(o.status.success());
        let s = String::from_utf8(o.stdout).unwrap();
        for needle in [
            "User=vesta",
            "KillSignal=SIGTERM",
            "KillMode=mixed",
            "SendSIGKILL=yes",
            "TimeoutStopSec=90",
            "host /var/lib/vesta/host.toml",
            "WantedBy=multi-user.target",
            "NoNewPrivileges=true",
            "ProtectSystem=strict",
            "ProtectHome=false",
        ] {
            assert!(s.contains(needle), "{needle}");
        }
        assert!(!s.contains("ProtectHome=read-only"));
        assert!(!s.contains("ProtectHome=true"));
        assert!(!s.contains("--user"));
        for open in ["/home", "/srv", "/opt", "/mnt", "/var"] {
            assert!(
                !s.contains(&format!("InaccessiblePaths={open}"))
                    && !s.contains(&format!("InaccessiblePaths= {open}")),
                "{open} must stay visible"
            );
        }
    }
    #[test]
    fn linux_rerun_preserves_existing_configuration() {
        let dir = tempfile::tempdir().unwrap();
        let decision = || {
            std::process::Command::new("bash")
                .args(["-c", SCRIPT, "test", "resume-decision"])
                .arg(dir.path())
                .output()
                .unwrap()
                .stdout
        };
        assert_eq!(decision(), b"create\n");
        std::fs::write(dir.path().join("host.toml"), "operator edits").unwrap();
        assert_eq!(decision(), b"preserve\n");
    }
    #[test]
    fn linux_host_update_never_enters_legacy_consumer_health() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("host")).unwrap();
        let error = crate::updates::run(&[
            "activate".into(),
            "--root".into(),
            root.path().display().to_string(),
        ])
        .unwrap_err();
        assert!(error.contains("Host-mode signed update trials"));
    }
    #[test]
    fn linux_import_root_list_and_remove_are_wired() {
        assert!(SCRIPT.contains("import_action=${1:-}"));
        assert!(SCRIPT.contains("add|list|remove)"));
        assert!(SCRIPT.contains("No import folders registered."));
        assert!(SCRIPT.contains("linux-acl remove"));
        assert!(SCRIPT.contains("Import root removed:"));
    }
    #[test]
    fn linux_options_reject_bad_ports_and_unsafe_roots_before_changes() {
        for args in [
            vec!["install", "/missing", "--port", "0"],
            vec!["install", "/missing", "--root", "/"],
            vec!["install", "/missing", "--unknown"],
        ] {
            assert!(
                !std::process::Command::new("bash")
                    .arg("-c")
                    .arg(SCRIPT)
                    .arg("test")
                    .args(args)
                    .status()
                    .unwrap()
                    .success()
            );
        }
    }
}
#[cfg(target_os = "linux")]
pub fn identity(root: &std::path::Path) -> Result<(), String> {
    require_root()?;
    if root.join("key.pem").exists() || root.join("certificate.pem").exists() {
        if root.join("key.pem").exists() && root.join("certificate.pem").exists() {
            return Ok(());
        }
        return Err(
            "Incomplete TLS identity; restore the missing identity file before rerunning".into(),
        );
    }
    let key = rcgen::KeyPair::generate().map_err(|e| e.to_string())?;
    let cert = rcgen::CertificateParams::new(vec!["localhost".into(), "127.0.0.1".into()])
        .map_err(|e| e.to_string())?
        .self_signed(&key)
        .map_err(|e| e.to_string())?;
    std::fs::write(root.join("key.pem"), key.serialize_pem()).map_err(|e| e.to_string())?;
    std::fs::write(root.join("certificate.pem"), cert.pem()).map_err(|e| e.to_string())
}
