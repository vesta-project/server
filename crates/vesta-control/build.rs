use std::{env, fs, path::PathBuf};

fn main() {
    let root = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("manifest directory"))
        .parent()
        .expect("crates directory")
        .parent()
        .expect("workspace directory")
        .to_owned();
    let local = root.join(".env");
    println!("cargo:rerun-if-changed={}", local.display());
    println!("cargo:rerun-if-env-changed=CURSEFORGE_API_KEY");
    println!("cargo:rerun-if-env-changed=VESTA_REQUIRE_PROVIDER_KEYS");
    // Read just the integration key; do not import unrelated launcher settings.
    let key = env::var("CURSEFORGE_API_KEY")
        .ok()
        .or_else(|| {
            dotenvy::from_path_iter(&local)
                .ok()?
                .filter_map(Result::ok)
                .find_map(|(name, value)| (name == "CURSEFORGE_API_KEY").then_some(value))
        })
        .unwrap_or_default();
    assert!(
        key.is_empty() || (key.len() <= 1024 && key.bytes().all(|b| (33..=126).contains(&b))),
        "CURSEFORGE_API_KEY is invalid; its value is redacted"
    );
    assert!(
        env::var("VESTA_REQUIRE_PROVIDER_KEYS").as_deref() != Ok("1") || !key.is_empty(),
        "CURSEFORGE_API_KEY must be configured for release builds"
    );
    // Match the launcher's application-key embedding. This is obfuscation, not
    // encryption: distributed application keys are recoverable from binaries.
    let bytes: Vec<u8> = key.bytes().map(|b| b ^ 0x55).collect();
    let output = PathBuf::from(env::var_os("OUT_DIR").expect("build output"));
    fs::write(
        output.join("provider_key.rs"),
        format!("const BUNDLED_CURSEFORGE_KEY: &[u8] = &{bytes:?};\n"),
    )
    .expect("write provider key module");
}
