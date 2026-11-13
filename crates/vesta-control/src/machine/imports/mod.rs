//! Host folder browsing, AMP candidate discovery and read-only inspection for
//! the owner import flow (`protocol/host-v1/import.md`). Plain Rust APIs; HTTP
//! routing belongs to H08. `jobs` plans and copies through `CopiedExport`. Nothing
//! here executes, writes, locks or reads credentials from a source folder.
pub mod amp;
pub mod browse;
pub mod inspection;
pub mod jobs;
mod properties;
mod arguments;

pub use amp::{CandidateScan, CandidatesResponse, ImportCandidate, ImportWarning, Remediation};
pub use browse::{
    CheckResponse, FolderBrowser, FolderEntry, FolderTokenResponse, ImportRootConfig, ListRequest, ListResponse,
    OwnerBinding, ResolveRequest, RootInfo, RootsResponse,
};
pub use inspection::{InspectRequest, InspectionReport, Loader};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ImportError {
    UnknownRoot,
    InvalidRoot,
    InvalidPath,
    InvalidCursor,
    TokenInvalid,
    TokenExpired,
    LinkedPath,
    ProtectedPath,
    MountBoundary,
    NotDirectory,
    NotFound,
    Unreadable,
    TooManyEntries,
    Changed,
    Unavailable,
}
impl ImportError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::UnknownRoot => "import_root_unknown",
            Self::InvalidRoot => "import_root_invalid",
            Self::InvalidPath => "folder_path_invalid",
            Self::InvalidCursor => "folder_cursor_invalid",
            Self::TokenInvalid => "folder_token_invalid",
            Self::TokenExpired => "folder_token_expired",
            Self::LinkedPath | Self::MountBoundary => "unsafe_path",
            Self::ProtectedPath => "folder_protected",
            Self::NotDirectory => "folder_not_directory",
            Self::NotFound => "folder_not_found",
            Self::Unreadable => "source_unreadable",
            Self::TooManyEntries => "folder_too_large",
            Self::Changed => "source_changed",
            Self::Unavailable => "folder_unavailable",
        }
    }
}
impl std::fmt::Display for ImportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code())
    }
}
impl std::error::Error for ImportError {}

#[cfg(test)]
mod tests;
