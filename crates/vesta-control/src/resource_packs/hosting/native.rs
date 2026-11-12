//! Existing host/runtime adapters. No alternate native properties writer.
use super::*;
use crate::{
    config::properties::ServerPropertiesEditor,
    files::{FileBrowser, StoppedGuard},
    operations::RuntimeHandle,
    software::SoftwareHandle,
};
pub struct NativeHooks {
    pub recovery: Mutex<Option<crate::operations::StartBlock>>,
    pub maintenance: Arc<tokio::sync::Mutex<()>>,
    pub db: StateDatabase,
    pub files: FileBrowser,
    pub runtime: RuntimeHandle,
    pub software: Option<SoftwareHandle>,
    pub fallback_minecraft_version: String,
    pub game: std::path::PathBuf,
}
impl RuntimeHooks for NativeHooks {
    fn recovery_block(&self, blocked: bool) -> Result<()> {
        let mut recovery = self.recovery.lock().map_err(|_| Error::Unavailable)?;
        if blocked && recovery.is_none() {
            *recovery = Some(self.runtime.block_starts());
        }
        if !blocked {
            recovery.take();
        }
        Ok(())
    }
    fn minecraft_version(&self) -> Result<String> {
        if let Some(software) = &self.software {
            let (_, version) = software.content_target().map_err(|_| Error::Unavailable)?;
            if let Some(version) = version {
                return Ok(version);
            }
        }
        Ok(self.fallback_minecraft_version.clone())
    }
    fn preview(
        &self,
        actor: &Subject,
        changes: &BTreeMap<String, String>,
    ) -> Result<(String, String, String)> {
        ServerPropertiesEditor::new(&self.files)
            .hosted_preview(actor, changes)
            .map_err(map_file)
    }
    fn apply(
        &self,
        actor: &Subject,
        key: &str,
        before: &str,
        expected_version: &str,
        changes: &BTreeMap<String, String>,
    ) -> Result<Box<dyn Send>> {
        let _maintenance = self
            .maintenance
            .clone()
            .try_lock_owned()
            .map_err(|_| Error::Busy)?;
        if self.minecraft_version()? != expected_version {
            return Err(Error::TargetChanged);
        }
        let hold =
            StoppedGuard::acquire(&self.runtime, self.db.directory()).map_err(|e| match e {
                crate::files::FileError::Conflict(_) => Error::StoppedRequired,
                other => map_file(other),
            })?;
        ServerPropertiesEditor::new(&self.files)
            .hosted_apply(actor, key, before, changes, hold.clone())
            .map_err(map_file)?;
        Ok(Box::new((hold, _maintenance)))
    }
    fn properties_hash(&self) -> Result<String> {
        let bytes = crate::files::read_local_document(
            &self.game.join("server.properties"),
            crate::minecraft_admission::MAX_SERVER_PROPERTIES_BYTES,
        )
        .map_err(map_file)?;
        Ok(hash(&bytes))
    }
}
fn map_file(e: crate::files::FileError) -> Error {
    use crate::files::FileError;
    match e {
        FileError::Conflict(_) => Error::Conflict,
        FileError::Control(_) => Error::Forbidden,
        FileError::RecoveryRequired(_) => Error::RecoveryRequired,
        _ => Error::Unavailable,
    }
}
