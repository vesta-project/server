//! Read-only upstream catalogue diagnostic; never installs or executes software.
use vesta_server_install::{
    InstallRoots, ServerInstaller, SilentProgressReporter, Software, TransportConfig,
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let roots = InstallRoots::new(temp.path(), &[])?;
    let installer = ServerInstaller::new(roots, TransportConfig::direct())?;
    for software in [
        Software::Vanilla,
        Software::Paper,
        Software::Folia,
        Software::Purpur,
        Software::Fabric,
        Software::Quilt,
        Software::Forge,
        Software::NeoForge,
    ] {
        match installer.versions(software, &SilentProgressReporter).await {
            Ok(versions) => {
                println!("{software:?}: {} Minecraft versions", versions.len());
                if let Some(version) = versions.first() {
                    match installer
                        .releases(software, version, &SilentProgressReporter)
                        .await
                    {
                        Ok(releases) => println!("  {version}: {} releases", releases.len()),
                        Err(error) => println!("  {version}: {error}"),
                    }
                }
            }
            Err(error) => println!("{software:?}: {error}"),
        }
    }
    Ok(())
}
