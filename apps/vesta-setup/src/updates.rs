//! OS-local signed update staging, fenced activation and exact prior-version recovery.
use std::{
    collections::BTreeMap,
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};
use vesta_update::{StartupPolicy, UpdateSource, UpdateStore};
/// Shared by both CLI entry points so their accepted actions cannot drift.
pub fn accepts_action(action: &str) -> bool {
    matches!(
        action,
        "init"
            | "status"
            | "check"
            | "stage"
            | "verify"
            | "configure"
            | "capabilities"
            | "probe"
            | "activate"
            | "recover"
            | "--help"
            | "-h"
    )
}
pub const HELP: &str = "vesta-setup update <init|status|check|stage|verify|configure|activate|recover> --root <installation>\n  init: --release-key <trusted public key hex> --channel stable|nightly\n  init/check/stage/configure: --bundle-dir <directory> OR --bundle-url <https-directory>\n                             [--asset-prefix <target->]\n  configure: --startup-check on|off --interval-seconds <positive integer>\n  verify: --manifest-sha256 <staged manifest hash> (offline fresh verification)\nInit requires update metadata for the exact installed release. Startup checks are opt-in, metadata-only and nonblocking. Stage/verify never activate or restart.\n  activate: --manifest-sha256 <hash> --idempotency-key <key> --restart on|off\n            requires a stopped, clean, registered first-server installation.\n  recover: --activation-id <id> (exact previous healthy build; stopped only)";
pub fn run(args: &[String]) -> Result<(), String> {
    if args == ["capabilities"] {
        println!(
            "{}",
            vesta_state::update_capabilities(vesta_control::onboarding::compiled_supervisor_ipc())
        );
        return Ok(());
    }
    let Some(action) = args.first().map(String::as_str) else {
        println!("{HELP}");
        return Ok(());
    };
    if matches!(action, "--help" | "-h") {
        println!("{HELP}");
        return Ok(());
    }
    if !accepts_action(action) {
        return Err("update_command_invalid".into());
    }
    let mut values = BTreeMap::new();
    let mut i = 1;
    while i < args.len() {
        let key = &args[i];
        if ![
            "--root",
            "--release-key",
            "--channel",
            "--bundle-dir",
            "--bundle-url",
            "--asset-prefix",
            "--manifest-sha256",
            "--startup-check",
            "--interval-seconds",
            "--probe-nonce",
            "--idempotency-key",
            "--activation-id",
            "--restart",
        ]
        .contains(&key.as_str())
        {
            return Err("update_option_invalid".into());
        }
        let value = args
            .get(i + 1)
            .filter(|v| !v.is_empty() && !v.starts_with('-'))
            .ok_or("update_option_value_required")?;
        if values.insert(key.as_str(), value.as_str()).is_some() {
            return Err("update_duplicate_option".into());
        }
        i += 2;
    }
    let root = PathBuf::from(*values.get("--root").ok_or("update_root_required")?);
    if action == "probe" {
        if values.len() != 3 {
            return Err("update_probe_options_invalid".into());
        }
        return crate::activation::probe(
            &root,
            values
                .get("--manifest-sha256")
                .ok_or("update_probe_input_invalid")?,
            values
                .get("--probe-nonce")
                .ok_or("update_probe_input_invalid")?,
        );
    }
    if values.contains_key("--probe-nonce") {
        return Err("update_option_invalid".into());
    }
    let _installation_lock = crate::engine::lock_existing_installation(&root)?;
    let receipt = crate::engine::read_receipt(&root)?;
    if root.join("private/uninstall-pending").exists() {
        return Err("update_uninstall_pending".into());
    }
    if action != "init"
        && (values.contains_key("--release-key") || values.contains_key("--channel"))
    {
        return Err("update_trust_override_refused".into());
    }
    if action == "status" && values.len() != 1 {
        return Err("update_status_options_invalid".into());
    }
    if matches!(action, "verify" | "activate") {
        if action == "activate" {
            if !values.contains_key("--manifest-sha256")
                || !values.contains_key("--idempotency-key")
                || !matches!(values.get("--restart").copied(), Some("on" | "off"))
            {
                return Err("update_activate_options_invalid".into());
            }
        } else {
            if values.len() != 2 || !values.contains_key("--manifest-sha256") {
                return Err("update_verify_options_invalid".into());
            }
        }
    } else if values.contains_key("--manifest-sha256") {
        return Err("update_option_invalid".into());
    }
    if action != "configure"
        && (values.contains_key("--startup-check") || values.contains_key("--interval-seconds"))
    {
        return Err("update_option_invalid".into());
    }
    let source = UpdateSource {
        local: values.get("--bundle-dir").map(PathBuf::from),
        base_url: values.get("--bundle-url").map(|s| s.to_string()),
        asset_prefix: values.get("--asset-prefix").map(|s| s.to_string()),
    };
    let now = i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| "update_clock_invalid")?
            .as_millis(),
    )
    .map_err(|_| "update_clock_invalid")?;
    let store = UpdateStore::open(&root.join("private/update-state"))?;
    if matches!(action, "activate" | "recover") {
        let allowed = if action == "activate" {
            vec![
                "--root",
                "--manifest-sha256",
                "--idempotency-key",
                "--restart",
                "--bundle-dir",
                "--bundle-url",
                "--asset-prefix",
            ]
        } else {
            vec!["--root", "--activation-id"]
        };
        if values.keys().any(|key| !allowed.contains(key)) {
            return Err("update_activate_options_invalid".into());
        }
        let lease = crate::host::acquire_host_lease(&root)?;
        let mut native = crate::activation::NativeHost::open(&root, &lease)?;
        if !store.registered_for(&receipt.installation_id)? {
            if action == "recover" {
                return Err("update_installation_not_registered".into());
            }
            if source.local.is_some() || source.base_url.is_some() {
                let installed = vesta_update::release::load(vesta_update::release::Source {
                    local: source.local.clone(),
                    base_url: source.base_url.clone(),
                    asset_prefix: source.asset_prefix.clone(),
                    manifest_sha256: receipt.release_manifest_sha256.clone(),
                    release_key: store.trust()?.release_key,
                })?;
                store.register_installed(&installed, &root, &mut native, now)?;
            } else {
                store.register_saved_installed(
                    &receipt.release_manifest_sha256,
                    &root,
                    &mut native,
                    now,
                )?;
            }
        }
        let result = if action == "activate" {
            store.prepare_activation(
                values["--manifest-sha256"],
                values["--idempotency-key"],
                &mut native,
                now,
            )?
        } else {
            store.recover_activation(
                values
                    .get("--activation-id")
                    .ok_or("update_activation_id_required")?,
                &mut native,
                now,
            )?
        };
        let mut bootstrap = if action == "activate"
            && values.get("--restart").copied() == Some("on")
            && result.state == "restart_pending"
        {
            let mut payload = store
                .active_payload(&receipt.installation_id, now)?
                .ok_or("update_installation_not_registered")?;
            let command = payload.command(if cfg!(windows) {
                "vesta-setup.exe"
            } else {
                "vesta-setup"
            })?;
            Some((payload, command))
        } else {
            None
        };
        drop(native);
        drop(lease);
        drop(_installation_lock);
        if let Some((_held, command)) = bootstrap.take() {
            crate::platform::start_verified_host(&root, command)?;
        }
        println!(
            "{}",
            serde_json::to_string_pretty(&result).map_err(|_| "update_output_failed")?
        );
        return Ok(());
    }
    if values.contains_key("--idempotency-key")
        || values.contains_key("--activation-id")
        || values.contains_key("--restart")
    {
        return Err("update_option_invalid".into());
    }
    let result=match action {
        "init"=>serde_json::to_value(store.initialize(values.get("--release-key").ok_or("update_release_key_required")?,values.get("--channel").ok_or("update_channel_required")?,&receipt.release_manifest_sha256,&source,now)?),
        "status"=>serde_json::to_value(serde_json::json!({"trust":store.trust()?,"stages":store.status()?,"startup_policy":store.startup_policy()?,"last_startup_check":store.last_startup_check()?,"activations":store.activations()?,"activation_supported":store.registered_for(&receipt.installation_id)?})),
        "check"=>serde_json::to_value(store.check(&source,now)?),
        "stage"=>serde_json::to_value(store.stage(&source,now,&mut |_,_,_|{})?),
        "verify"=>serde_json::to_value(store.verify_staged(values["--manifest-sha256"],now)?),
        "configure"=>{
            let check_on_start=match values.get("--startup-check").copied(){Some("on")=>true,Some("off")=>false,_=>return Err("update_startup_check_required".into())};
            let policy=if !check_on_start && values.len()==2 {
                let mut policy=store.startup_policy()?.ok_or("update_startup_not_configured")?;
                policy.check_on_start=false;policy
            }else{
                let minimum_interval_ms=values.get("--interval-seconds").and_then(|s|s.parse::<u64>().ok()).and_then(|s|s.checked_mul(1000)).ok_or("update_startup_interval_invalid")?;
                StartupPolicy{check_on_start,minimum_interval_ms,source}
            };
            store.configure_startup_check(&policy)?;
            serde_json::to_value(policy)
        },
        _=>return Err("update_command_invalid".into()),
    }.map_err(|_|"update_output_failed")?;
    println!(
        "{}",
        serde_json::to_string_pretty(&result).map_err(|_| "update_output_failed")?
    );
    Ok(())
}

/// Only the installed host calls this with its validated receipt/root. An
/// absent update store makes ordinary startup entirely network-free.
pub fn start_check(root: &std::path::Path, installed_manifest: &str) {
    let state = root.join("private/update-state");
    if !state.exists() {
        return;
    }
    let manifest = installed_manifest.to_owned();
    // No startup wait, raw transport logs, executable staging, or service restart.
    // One attempt is admitted under the shared update lock and durably spaced.
    let _ = std::thread::Builder::new()
        .name("vesta-update-check".into())
        .spawn(move || {
            let Some(now) = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .ok()
                .and_then(|d| i64::try_from(d.as_millis()).ok())
            else {
                return;
            };
            if let Ok(store) = UpdateStore::open(&state) {
                let _ = store.check_on_start(&manifest, now);
            }
        });
}

/// Stable OS-local CLI bootstrap; only an already committed healthy selection is
/// dispatched. Pending candidates use the separately fenced host runner instead.
pub fn dispatch_active(
    name: &str,
    args: &[std::ffi::OsString],
) -> Result<Option<std::process::ExitStatus>, String> {
    if args.first().is_some_and(|a| a == "host")
        || (args.first().is_some_and(|a| a == "update")
            && args
                .get(1)
                .is_some_and(|a| a == "probe" || a == "capabilities"))
        || std::env::var_os("VESTA_UPDATE_READINESS_NONCE").is_some()
    {
        return Ok(None);
    }
    if !["vesta-setup", "vesta-agent", "vesta"].contains(&name) {
        return Err("update_dispatch_name_invalid".into());
    }
    let current = std::fs::canonicalize(
        std::env::current_exe().map_err(|_| "update_executable_unavailable")?,
    )
    .map_err(|_| "update_executable_unavailable")?;
    let root = if let Some(root) = std::env::var_os("VESTA_INSTALLATION_ROOT") {
        PathBuf::from(root)
    } else if current
        .parent()
        .and_then(std::path::Path::file_name)
        .is_some_and(|p| p == "bin")
    {
        current
            .parent()
            .and_then(std::path::Path::parent)
            .ok_or("update_dispatch_root_invalid")?
            .to_owned()
    } else {
        return Ok(None);
    };
    if !root.join("private/setup.json").exists() {
        return Ok(None);
    }
    let receipt = crate::engine::read_receipt(&root)?;
    if !root.join("private/update-state").exists() {
        return Ok(None);
    }
    let store = UpdateStore::open(&root.join("private/update-state"))?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|t| i64::try_from(t.as_millis()).ok())
        .ok_or("update_clock_invalid")?;
    let Some(mut payload) = store.active_payload(&receipt.installation_id, now)? else {
        return Ok(None);
    };
    let executable = if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_owned()
    };
    let selected = std::fs::canonicalize(payload.directory().join("bin").join(&executable))
        .map_err(|_| "update_executable_unavailable")?;
    if crate::paths::comparable(&current) == crate::paths::comparable(&selected) {
        return Ok(None);
    }
    let mut command = payload.command(&executable)?;
    command.args(args).env("VESTA_INSTALLATION_ROOT", &root);
    command
        .status()
        .map(Some)
        .map_err(|_| "update_dispatch_failed".into())
}
