//! Trusted host integration. No implementation is selected by remote input.
use super::Result;
use vesta_server_install::ManagedLaunch;

/// A held shared start gate plus (when configured) supervisor stopped-game hold.
/// Acquiring this must refuse a live/draining/uncertain process and pending restart.
/// Dropping releases exclusion. `activate` must replace both the protected launch
/// document and the process owner's launch, without starting/stopping Minecraft.
pub trait StoppedGuard: Send {
    fn activate(&mut self, document: &str) -> Result<()>;
}

pub trait RuntimeHooks: Send + Sync + 'static {
    /// Translate backend-owned Java/argument paths to a bounded local launch TOML,
    /// preserving locally approved heap/JVM flags. Pure; do not activate or launch.
    fn launch_document(&self, launch: &ManagedLaunch) -> Result<String>;
    fn acquire_stopped(&self) -> Result<Box<dyn StoppedGuard>>;
    /// Persistent ambiguous activation must block every start/crash restart before
    /// automation is enabled. `false` follows a durably completed explicit rollback.
    fn recovery_block(&self, blocked: bool) -> Result<()>;
}
