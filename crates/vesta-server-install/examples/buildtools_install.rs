//! Opt-in official source-build smoke test. Never starts Minecraft or accepts its EULA.
use std::path::PathBuf;
use vesta_server_install::*;
struct Progress;
impl ProgressReporter for Progress {
    fn start_step(&self, name: &str, _: Option<u32>) {
        println!("{name}");
    }
    fn update_bytes(&self, _: u64, _: Option<u64>) {}
    fn done(&self, success: bool, message: Option<&str>) {
        println!("done={success} {message:?}");
    }
    fn is_cancelled(&self) -> bool {
        false
    }
    fn is_paused(&self) -> bool {
        false
    }
}
#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.len() != 5 || args[0] != "--build" {
        return Err("usage: buildtools_install --build <bukkit|spigot> <minecraft> <absolute-java> <existing-empty-install-base>".into());
    }
    let target = match args[1].as_str() {
        "bukkit" => Software::Bukkit,
        "spigot" => Software::Spigot,
        _ => return Err("target".into()),
    };
    let roots = InstallRoots::new(&PathBuf::from(&args[4]), &[])?;
    let installer = ServerInstaller::new(roots, TransportConfig::direct())?;
    let plan = installer
        .plan(
            target,
            &Selection {
                version: Some(args[2].clone()),
                ..Selection::default()
            },
            &PathBuf::from(&args[3]),
            &Progress,
        )
        .await?;
    println!(
        "reviewed {} {} build {:?}; destination {}",
        target.id(),
        plan.version(),
        plan.build(),
        plan.destination().display()
    );
    let approval = Approval::review(
        &plan,
        Acknowledgments {
            warnings: true,
            legacy_checksum: false,
        },
    )?;
    installer.install(&plan, approval, &Progress).await?;
    let receipt = installer.verify(&plan).await?;
    println!("VERIFIED {:?}", receipt.built.map(|r| r.output));
    Ok(())
}
