//! Explicit OS-local trust bootstrap and staging. No executable activation.
use std::{
    collections::BTreeMap,
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};
use vesta_update::{StartupPolicy, UpdateSource, UpdateStore};
/// Shared by both CLI entry points so their accepted actions cannot drift.
pub fn accepts_action(action: &str) -> bool {
    matches!(action, "init" | "status" | "check" | "stage" | "verify" | "configure" | "--help" | "-h")
}
pub const HELP: &str = "vesta-setup update <init|status|check|stage|verify|configure> --root <installation>\n  init: --release-key <trusted public key hex> --channel stable|nightly\n  init/check/stage/configure: --bundle-dir <directory> OR --bundle-url <https-directory>\n                             [--asset-prefix <target->]\n  configure: --startup-check on|off --interval-seconds <positive integer>\n  verify: --manifest-sha256 <staged manifest hash> (offline fresh verification)\nInit requires update metadata for the exact installed release. Startup checks are opt-in, metadata-only and nonblocking. Stage/verify never activate or restart.";
pub fn run(args: &[String]) -> Result<(), String> {
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
    if action == "verify" {
        if values.len()!=2 || !values.contains_key("--manifest-sha256") {
            return Err("update_verify_options_invalid".into());
        }
    } else if values.contains_key("--manifest-sha256") {
        return Err("update_option_invalid".into());
    }
    if action != "configure" && (values.contains_key("--startup-check") || values.contains_key("--interval-seconds")) {
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
    let result=match action {
        "init"=>serde_json::to_value(store.initialize(values.get("--release-key").ok_or("update_release_key_required")?,values.get("--channel").ok_or("update_channel_required")?,&receipt.release_manifest_sha256,&source,now)?),
        "status"=>serde_json::to_value(serde_json::json!({"trust":store.trust()?,"stages":store.status()?,"startup_policy":store.startup_policy()?,"last_startup_check":store.last_startup_check()?,"activation_supported":false})),
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
    if !state.exists() { return; }
    let manifest = installed_manifest.to_owned();
    // No startup wait, raw transport logs, executable staging, or service restart.
    // One attempt is admitted under the shared update lock and durably spaced.
    let _ = std::thread::Builder::new().name("vesta-update-check".into()).spawn(move || {
        let Some(now) = SystemTime::now().duration_since(UNIX_EPOCH).ok().and_then(|d|i64::try_from(d.as_millis()).ok()) else { return; };
        if let Ok(store) = UpdateStore::open(&state) { let _ = store.check_on_start(&manifest, now); }
    });
}
