use serde::{Deserialize, Serialize};
use vesta_server_install::{JavaRequirement, Software, SoftwareDescriptor, Warning};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SoftwareError {
    Forbidden,
    Disabled,
    InvalidRequest,
    Busy,
    PlanExpired,
    ConfirmationInvalid,
    RevisionConflict,
    IdempotencyConflict,
    PreviousUnavailable,
    RecoveryRequired,
    StateUnavailable,
    RuntimeUnavailable,
    Install(vesta_server_install::InstallError),
}
impl SoftwareError {
    pub fn code(self) -> &'static str {
        match self {
            Self::Forbidden => "software_forbidden",
            Self::Disabled => "software_disabled",
            Self::InvalidRequest => "software_invalid_request",
            Self::Busy => "software_busy",
            Self::PlanExpired => "software_plan_expired",
            Self::ConfirmationInvalid => "software_confirmation_invalid",
            Self::RevisionConflict => "software_revision_conflict",
            Self::IdempotencyConflict => "software_idempotency_conflict",
            Self::PreviousUnavailable => "software_previous_unavailable",
            Self::RecoveryRequired => "software_recovery_required",
            Self::StateUnavailable => "software_state_unavailable",
            Self::RuntimeUnavailable => "software_runtime_unavailable",
            Self::Install(e) => match e {
                vesta_server_install::InstallError::InvalidInput => "server_install.invalid_input",
                vesta_server_install::InstallError::Unavailable => "server_install.unavailable",
                vesta_server_install::InstallError::InvalidMetadata => {
                    "server_install.invalid_metadata"
                }
                vesta_server_install::InstallError::SourceRefused => {
                    "server_install.source_refused"
                }
                vesta_server_install::InstallError::LimitExceeded => {
                    "server_install.limit_exceeded"
                }
                vesta_server_install::InstallError::TransportFailed => {
                    "server_install.transport_failed"
                }
                vesta_server_install::InstallError::ArtifactChanged => {
                    "server_install.artifact_changed"
                }
                vesta_server_install::InstallError::IntegrityConflict => {
                    "server_install.integrity_conflict"
                }
                vesta_server_install::InstallError::Deadline => "server_install.deadline",
                vesta_server_install::InstallError::Cancelled => "server_install.cancelled",
                vesta_server_install::InstallError::ApprovalRequired => {
                    "server_install.approval_required"
                }
                vesta_server_install::InstallError::ApprovalMismatch => {
                    "server_install.approval_mismatch"
                }
                vesta_server_install::InstallError::Expired => "server_install.expired",
                vesta_server_install::InstallError::RuntimeUnknown => {
                    "server_install.runtime_unknown"
                }
                vesta_server_install::InstallError::RuntimeIncompatible => {
                    "server_install.runtime_incompatible"
                }
                vesta_server_install::InstallError::RuntimeFailed => {
                    "server_install.runtime_failed"
                }
                vesta_server_install::InstallError::PathRefused => "server_install.path_refused",
                vesta_server_install::InstallError::DestinationExists => {
                    "server_install.destination_exists"
                }
                vesta_server_install::InstallError::StorageUnknown => {
                    "server_install.storage_unknown"
                }
                vesta_server_install::InstallError::StorageLow => "server_install.storage_low",
                vesta_server_install::InstallError::Busy => "server_install.busy",
                vesta_server_install::InstallError::PublishFailed => {
                    "server_install.publish_failed"
                }
                vesta_server_install::InstallError::ReceiptInvalid => {
                    "server_install.receipt_invalid"
                }
            },
        }
    }
}
impl std::fmt::Display for SoftwareError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code())
    }
}
impl std::error::Error for SoftwareError {}
impl From<vesta_state::StateError> for SoftwareError {
    fn from(_: vesta_state::StateError) -> Self {
        Self::StateUnavailable
    }
}
impl From<rusqlite::Error> for SoftwareError {
    fn from(_: rusqlite::Error) -> Self {
        Self::StateUnavailable
    }
}
impl From<vesta_server_install::InstallError> for SoftwareError {
    fn from(e: vesta_server_install::InstallError) -> Self {
        Self::Install(e)
    }
}
pub type Result<T> = std::result::Result<T, SoftwareError>;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PlanRequest {
    pub software: Software,
    pub version: String,
    pub build: Option<u32>,
    pub loader_version: Option<String>,
    #[serde(default)]
    pub experimental: bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApplyRequest {
    pub plan_id: String,
    pub confirmation_token: String,
    pub idempotency_key: String,
    pub acknowledge_warnings: bool,
    pub acknowledge_legacy_checksum: bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RollbackRequest {
    pub expected_revision: u64,
    pub idempotency_key: String,
    pub confirm: bool,
    pub plan_id: String,
    pub confirmation_token: String,
}
#[derive(Serialize)]
pub struct PreparedRollback {
    pub plan_id: String,
    pub confirmation_token: String,
    pub expires_at_ms: i64,
    pub expected_revision: u64,
    pub target: Installation,
    pub risks: Vec<&'static str>,
}
#[derive(Serialize)]
pub struct PreparedPlan {
    pub plan_id: String,
    pub confirmation_token: String,
    pub expires_at_ms: i64,
    pub expected_revision: u64,
    pub software: Software,
    pub version: String,
    pub build: Option<u32>,
    pub java: JavaRequirement,
    pub java_compatible: bool,
    pub selected_java_major: u32,
    pub managed_download_major: Option<u32>,
    pub warnings: Vec<Warning>,
    pub risks: Vec<&'static str>,
}
#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
pub struct Installation {
    pub software: Option<Software>,
    pub version: Option<String>,
    pub build: Option<u32>,
    pub installation_id: String,
}
#[derive(Serialize)]
pub struct Status {
    pub available: bool,
    pub unavailable_reason: Option<&'static str>,
    pub revision: u64,
    pub current: Installation,
    pub previous: Option<Installation>,
    pub recovery_required: bool,
    pub catalog: Vec<SoftwareDescriptor>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Job {
    pub id: String,
    pub kind: String,
    pub state: String,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub error: Option<String>,
}

/// Protected internal state. Never serialize this into a remote response.
#[derive(Clone, Deserialize, Serialize)]
pub(super) struct StoredLaunch {
    pub installation: Installation,
    pub document: String,
    #[serde(default)]
    pub artifacts: Vec<vesta_server_install::ManagedArtifact>,
}
