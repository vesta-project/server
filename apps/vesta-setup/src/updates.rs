//! Explicit OS-local trust bootstrap and staging. No executable activation.
use std::{
    collections::BTreeMap,
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};
use vesta_update::{UpdateSource, UpdateStore};
pub const HELP: &str = "vesta-setup update <init|status|check|stage> --root <installation>\n  init: --release-key <trusted public key hex> --channel stable|nightly\n  init/check/stage: --bundle-dir <directory> OR --bundle-url <https-directory>\n                   [--asset-prefix <target->]\nInit requires update metadata for the exact installed release. Stage never activates or restarts.";
pub fn run(args: &[String]) -> Result<(), String> {
    let Some(action) = args.first().map(String::as_str) else {
        println!("{HELP}");
        return Ok(());
    };
    if matches!(action, "--help" | "-h") {
        println!("{HELP}");
        return Ok(());
    }
    if !matches!(action, "init" | "status" | "check" | "stage") {
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
        "status"=>serde_json::to_value(serde_json::json!({"trust":store.trust()?,"stages":store.status()?,"activation_supported":false})),
        "check"=>serde_json::to_value(store.check(&source,now)?),
        "stage"=>serde_json::to_value(store.stage(&source,now,&mut |_,_,_|{})?),
        _=>return Err("update_command_invalid".into()),
    }.map_err(|_|"update_output_failed")?;
    println!(
        "{}",
        serde_json::to_string_pretty(&result).map_err(|_| "update_output_failed")?
    );
    Ok(())
}
