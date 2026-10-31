use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ContentKind {
    #[default]
    Mod,
    Plugin,
    ResourcePack,
    Shader,
    Datapack,
}
impl ContentKind {
    pub fn directory(self, software: Option<&str>, world: &str) -> super::Result<String> {
        match self {
            Self::Mod if matches!(software, Some("fabric" | "quilt" | "forge" | "neoforge")) => {
                Ok("mods".into())
            }
            Self::Plugin
                if matches!(
                    software,
                    Some("paper" | "spigot" | "bukkit" | "purpur" | "folia")
                ) =>
            {
                Ok("plugins".into())
            }
            Self::ResourcePack => Ok("resourcepacks".into()),
            Self::Shader => Ok("shaderpacks".into()),
            Self::Datapack => {
                crate::files::RelativePath::parse(world).map_err(super::ContentError::File)?;
                Ok(format!("{world}/datapacks"))
            }
            Self::Mod => Err(super::ContentError::ModsUnsupported),
            Self::Plugin => Err(super::ContentError::PluginsUnsupported),
        }
    }
}
#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ContentProvider {
    pub provider: String,
    pub project_id: String,
    pub version_id: String,
}
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct DeclaredMetadata {
    pub loader: Option<String>,
    pub side: Option<String>,
    pub name: Option<String>,
    pub version: Option<String>,
    pub dependencies: Vec<String>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ContentEntry {
    #[serde(default)]
    pub modpack_owned: bool,
    pub kind: ContentKind,
    pub path: String,
    pub filename: String,
    pub size: String,
    pub enabled: Option<bool>,
    pub sha256: Option<String>,
    pub provider: Option<ContentProvider>,
    pub declared: Option<DeclaredMetadata>,
    pub pending: Option<String>,
    pub pending_id: Option<String>,
    pub pending_error: Option<String>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum ContentMutation {
    Remove {
        kind: ContentKind,
        path: String,
        expected_sha256: String,
    },
    Enable {
        kind: ContentKind,
        path: String,
        expected_sha256: String,
    },
    Disable {
        kind: ContentKind,
        path: String,
        expected_sha256: String,
    },
}
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ContentChange {
    pub trash_path: Option<String>,
    pub id: String,
    pub state: String,
    pub pending: Option<String>,
    pub path: String,
    pub error: Option<String>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ContentCapabilities {
    pub list: bool,
    pub install: bool,
    pub update: bool,
    pub remove: bool,
    pub toggle: bool,
    pub cancel: bool,
    pub datapack_toggle_offline: bool,
    pub datapack_toggle_running: bool,
    pub mods_supported: bool,
    pub plugins_supported: bool,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ContentInventory {
    /// Actual journaled writes waiting for stopped-process ownership, not loaded files.
    pub deferred_writes: usize,
    pub needs_restart: bool,
    pub items: Vec<ContentEntry>,
    pub capabilities: ContentCapabilities,
    pub minecraft_version: String,
    pub software: Option<String>,
    pub truncated: bool,
}
