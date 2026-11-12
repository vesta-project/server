use super::super::{PackFormat, PublishedPack};
use serde::{Deserialize, Serialize};

pub type Result<T> = std::result::Result<T, Error>;
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    Invalid,
    Forbidden,
    Conflict,
    IdempotencyConflict,
    Busy,
    Limit,
    Unavailable,
    NotFound,
    Confirmation,
    StoppedRequired,
    RecoveryRequired,
    TargetChanged,
    Build(super::super::Error),
}
impl Error {
    pub fn code(self) -> &'static str {
        match self {
            Self::Invalid => "resource_pack_request_invalid",
            Self::Forbidden => "resource_pack_forbidden",
            Self::Conflict => "resource_pack_revision_conflict",
            Self::IdempotencyConflict => "resource_pack_idempotency_conflict",
            Self::Busy => "resource_pack_busy",
            Self::Limit => "resource_pack_storage_limit",
            Self::Unavailable => "resource_pack_unavailable",
            Self::NotFound => "resource_pack_not_found",
            Self::Confirmation => "resource_pack_confirmation_invalid",
            Self::StoppedRequired => "resource_pack_requires_stopped_game",
            Self::RecoveryRequired => "resource_pack_recovery_required",
            Self::TargetChanged => "resource_pack_target_changed",
            Self::Build(e) => e.code(),
        }
    }
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code())
    }
}
impl std::error::Error for Error {}
impl From<rusqlite::Error> for Error {
    fn from(_: rusqlite::Error) -> Self {
        Self::Unavailable
    }
}
impl From<vesta_state::StateError> for Error {
    fn from(_: vesta_state::StateError) -> Self {
        Self::Unavailable
    }
}
impl From<vesta_state::TransactionError<Error>> for Error {
    fn from(e: vesta_state::TransactionError<Error>) -> Self {
        match e {
            vesta_state::TransactionError::Work(e) => e,
            _ => Self::Unavailable,
        }
    }
}
impl From<std::io::Error> for Error {
    fn from(_: std::io::Error) -> Self {
        Self::Unavailable
    }
}
impl From<super::super::Error> for Error {
    fn from(e: super::super::Error) -> Self {
        Self::Build(e)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Source {
    Provider {
        provider: String,
        project_id: String,
        version_id: String,
    },
    Upload {
        sha256: String,
    },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Layer {
    pub id: String,
    pub source: Source,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    /// Explicitly configured HTTPS origin, with any reverse proxy forwarding only
    /// /resource-packs/*.zip. It is never fetched or treated as a management URL.
    pub public_origin: String,
    pub minecraft_version: String,
    pub format: PackFormat,
    pub description: String,
    pub required: bool,
    /// Plain text, serialized as a Minecraft text component at activation.
    pub prompt: String,
    /// Lowest priority first, highest last.
    pub layers: Vec<Layer>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Draft {
    pub revision: String,
    pub pack_id: String,
    pub settings: Option<Settings>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Save {
    pub expected_revision: String,
    pub settings: Settings,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BuildRequest {
    pub expected_revision: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Release {
    pub id: String,
    pub draft_revision: String,
    pub pack_id: String,
    pub settings: Settings,
    pub artifact: PublishedPack,
    pub url: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Job {
    pub id: String,
    pub state: String,
    pub step: String,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub error: Option<String>,
    pub release: Option<Release>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Status {
    pub draft: Draft,
    pub active: Option<Release>,
    pub jobs: Vec<Job>,
    pub recovery_required: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlanRequest {
    pub release_id: String,
    pub expected_revision: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Plan {
    pub plan_id: String,
    pub confirmation_token: String,
    pub expires_at_ms: i64,
    pub release: Release,
    pub replaces_external_pack: bool,
    pub restart_needed: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Apply {
    pub plan_id: String,
    pub confirmation_token: String,
    pub acknowledge_external_pack: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Applied {
    pub release: Release,
    pub restart_needed: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AbandonRecovery {
    pub expected_properties_sha256: String,
    pub confirm_abandon: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UploadBegin {
    pub size: String,
    pub sha256: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UploadChunk {
    pub upload_id: String,
    pub offset: String,
    pub content_base64: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Upload {
    pub id: String,
    pub size: String,
    pub received: String,
    pub sha256: String,
}
