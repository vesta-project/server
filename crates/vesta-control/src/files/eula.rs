//! Explicit EULA consent, persisted through the ordinary recoverable file journal.
use super::*;
use serde_json::{Value, json};
impl FileBrowser {
    pub fn eula(&self, subject: &Subject) -> Result<bool> {
        self.check_global_authority(subject, Capability::ConfigView)?;
        let path = RelativePath::parse("eula.txt")?;
        match self.read(subject, &path, 8192) {
            Ok(file) => Ok(accepted(file.bytes())),
            Err(FileError::NotFound) => Ok(false),
            Err(error) => Err(error),
        }
    }
    pub fn set_eula(&self, subject: &Subject, key: &str, accepted: bool) -> Result<Value> {
        self.check_global_authority(subject, Capability::ConfigEdit)?;
        let path = RelativePath::parse("eula.txt")?;
        self.for_remote().execute_remote(
            subject,
            key,
            json!(["eula", accepted]),
            || Ok(vec![(Capability::ConfigEdit.id().into(), String::new())]),
            |files| {
                let mut files = files.clone();
                files.remote_outcome = Some(json!({"accepted": accepted}));
                let condition = match files.read(subject, &path, 8192) {
                    Ok(file) => WritePrecondition::Matches(file.sha256().into()),
                    Err(FileError::NotFound) => WritePrecondition::Absent,
                    Err(error) => return Err(error),
                };
                let bytes =
                    format!("# Minecraft EULA: https://aka.ms/MinecraftEULA\neula={accepted}\n");
                files.write(subject, &path, &condition, bytes.as_bytes())?;
                Ok(json!({"accepted": accepted}))
            },
        )
    }
}
fn accepted(bytes: &[u8]) -> bool {
    let text = String::from_utf8_lossy(bytes);
    text.lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.starts_with('#') || line.starts_with('!') {
                return None;
            }
            let (key, value) = line.split_once('=')?;
            (key.trim() == "eula").then(|| value.trim() == "true")
        })
        .next_back()
        .unwrap_or(false)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn explicit_true_only_and_last_setting_wins() {
        assert!(!accepted(b"# eula=true\neula=false\n"));
        assert!(accepted(b"# EULA\neula = true\n"));
        assert!(!accepted(b"eula=true\neula=false\n"));
        assert!(!accepted(b""));
    }
}
