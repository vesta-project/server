//! Read-only upstream catalogue diagnostic; never installs or executes software.
use std::time::Instant;
use vesta_server_install::{
    InstallRoots, ServerInstaller, SilentProgressReporter, Software, TransportConfig,
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let roots = InstallRoots::new(temp.path(), &[])?;
    let installer = ServerInstaller::new(roots, TransportConfig::direct())?;
    let mut failures = 0;
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
        let started = Instant::now();
        match installer.versions(software, &SilentProgressReporter).await {
            Ok(versions) => {
                println!(
                    "{software:?}: {} Minecraft versions in {:?}",
                    versions.len(),
                    started.elapsed()
                );
                let warm = Instant::now();
                match installer.versions(software, &SilentProgressReporter).await {
                    Ok(values) => println!(
                        "  repeated versions: {} in {:?}",
                        values.len(),
                        warm.elapsed()
                    ),
                    Err(error) => {
                        failures += 1;
                        println!("  repeated versions: {error}");
                    }
                }
                // Providers do not all return the same ordering. Sample the
                // newest numeric release, not an arbitrary first entry.
                let newest = versions
                    .iter()
                    .filter_map(|version| {
                        version
                            .split('.')
                            .map(str::parse::<u32>)
                            .collect::<std::result::Result<Vec<_>, _>>()
                            .ok()
                            .map(|parts| (parts, version))
                    })
                    .max_by(|a, b| a.0.cmp(&b.0))
                    .map(|(_, version)| version)
                    .or_else(|| versions.first());
                if let Some(version) = newest {
                    let started = Instant::now();
                    match installer
                        .releases(software, version, &SilentProgressReporter)
                        .await
                    {
                        Ok(releases) => println!(
                            "  {version}: {} releases in {:?}",
                            releases.len(),
                            started.elapsed()
                        ),
                        Err(error) => {
                            failures += 1;
                            println!("  {version}: {error} after {:?}", started.elapsed());
                        }
                    }
                }
            }
            Err(error) => {
                failures += 1;
                println!("{software:?}: {error} after {:?}", started.elapsed());
            }
        }
    }
    if failures != 0 {
        return Err(format!("{failures} catalogue requests failed").into());
    }
    Ok(())
}
