//! Launch an installed, reverified distribution against a separately owned game tree.
//! Loader classpaths are absolute: the fresh-install argfile assumes its own game cwd.
use crate::{InstallError, InstallPlan, Result, ServerInstaller};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use tokio::io::AsyncReadExt;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManagedArtifact {
    pub path: PathBuf,
    pub size: u64,
    pub sha256: String,
}

#[derive(Clone, Debug)]
pub struct ManagedLaunch {
    pub java: PathBuf,
    pub working_directory: PathBuf,
    pub args: Vec<String>,
    pub required_files: Vec<PathBuf>,
    pub artifacts: Vec<ManagedArtifact>,
}

impl ServerInstaller {
    pub fn validate_managed_game(&self, game: &Path) -> Result<PathBuf> {
        self.roots.recheck()?;
        crate::install::guard(game)?;
        let game = dunce::canonicalize(game).map_err(|_| InstallError::PathRefused)?;
        if !game.is_dir()
            || crate::install::overlap(&game, &self.roots.base)
            || game
                .to_str()
                .is_none_or(|s| s.chars().any(char::is_control))
        {
            return Err(InstallError::PathRefused);
        }
        Ok(game)
    }
    /// Trusted-local game path only. This never writes the game tree or launches Java.
    /// Artifact verification runs before generating a candidate launch definition.
    pub async fn managed_launch(&self, plan: &InstallPlan, game: &Path) -> Result<ManagedLaunch> {
        let receipt = self.verify(plan).await?;
        plan.runtime.recheck()?;
        let mut launch = self.managed_launch_checked(plan, game)?;
        launch.artifacts = if let Some(bundle) = receipt.bundle {
            bundle
                .artifacts
                .into_iter()
                .map(|a| ManagedArtifact {
                    path: plan.destination().join(a.path),
                    size: a.actual_size,
                    sha256: a.observed_sha256,
                })
                .collect()
        } else {
            vec![ManagedArtifact {
                path: plan.destination().join(plan.software.jar_path()),
                size: receipt.actual_size,
                sha256: receipt.observed_sha256,
            }]
        };
        Ok(launch)
    }

    fn managed_launch_checked(&self, plan: &InstallPlan, game: &Path) -> Result<ManagedLaunch> {
        let game = self.validate_managed_game(game)?;
        let root = plan.destination();
        let mut args = Vec::new();
        let required_files;
        if let Some(bundle) = &plan.bundle {
            crate::bundle::validate(plan.software, bundle, plan.limits)?;
            let paths: Vec<_> = bundle
                .artifacts
                .iter()
                .map(|a| root.join(&a.destination))
                .collect();
            let separator = if cfg!(windows) { ";" } else { ":" };
            // A classpath has platform delimiters without an escaping convention.
            if paths
                .iter()
                .any(|p| p.to_str().is_none_or(|s| s.contains(separator)))
            {
                return Err(InstallError::PathRefused);
            }
            let classpath = bundle
                .classpath
                .iter()
                .map(|&i| {
                    paths
                        .get(i)
                        .and_then(|p| p.to_str())
                        .ok_or(InstallError::InvalidInput)
                })
                .collect::<Result<Vec<_>>>()?
                .join(separator);
            let property = if plan.software == crate::Software::Fabric {
                "fabric"
            } else {
                "loader"
            };
            args.push(format!(
                "-D{property}.gameJarPath={}",
                paths[0].to_str().ok_or(InstallError::PathRefused)?
            ));
            args.extend([
                "-cp".into(),
                classpath,
                crate::bundle::server_main(plan.software)?.into(),
                "nogui".into(),
            ]);
            required_files = paths;
        } else {
            let jar = root.join(plan.software.jar_path());
            args.extend([
                "-jar".into(),
                jar.to_str().ok_or(InstallError::PathRefused)?.into(),
                "nogui".into(),
            ]);
            required_files = vec![jar];
        }
        Ok(ManagedLaunch {
            java: plan.runtime.path.clone(),
            working_directory: game,
            args,
            required_files,
            artifacts: Vec::new(),
        })
    }
}

/// Reverify protected, service-persisted rollback evidence. Never expose this as
/// an API accepting caller-provided paths/digests. Bounded streaming, no writes.
pub async fn verify_managed_artifacts(artifacts: &[ManagedArtifact]) -> Result<()> {
    tokio::time::timeout(std::time::Duration::from_secs(600), async {
        if artifacts.len() > 129 {
            return Err(InstallError::LimitExceeded);
        }
        let mut total = 0_u64;
        for artifact in artifacts {
            total = total
                .checked_add(artifact.size)
                .ok_or(InstallError::LimitExceeded)?;
            if total > 4 * 1024 * 1024 * 1024
                || artifact.size == 0
                || artifact.size > 1024 * 1024 * 1024
                || !artifact.path.is_absolute()
                || artifact.sha256.len() != 64
                || !artifact.sha256.bytes().all(|c| c.is_ascii_hexdigit())
            {
                return Err(InstallError::ReceiptInvalid);
            }
            crate::install::plain_file(&artifact.path)?;
            let mut file = tokio::fs::File::open(&artifact.path)
                .await
                .map_err(|_| InstallError::ReceiptInvalid)?;
            let mut hash = Sha256::new();
            let mut size = 0_u64;
            let mut buffer = [0_u8; 64 * 1024];
            loop {
                let n = file
                    .read(&mut buffer)
                    .await
                    .map_err(|_| InstallError::ReceiptInvalid)?;
                if n == 0 {
                    break;
                }
                size = size
                    .checked_add(n as u64)
                    .ok_or(InstallError::LimitExceeded)?;
                if size > artifact.size {
                    return Err(InstallError::ArtifactChanged);
                }
                hash.update(&buffer[..n]);
            }
            if size != artifact.size || format!("{:x}", hash.finalize()) != artifact.sha256 {
                return Err(InstallError::ArtifactChanged);
            }
            crate::install::plain_file(&artifact.path)?;
        }
        Ok(())
    })
    .await
    .map_err(|_| InstallError::Deadline)?
}
