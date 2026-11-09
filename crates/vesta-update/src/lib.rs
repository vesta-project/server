//! Signed release trust and side-by-side staging. Never activates executables.
mod metadata;
pub mod release;
mod store;
mod verification;
pub mod rollout;
pub use metadata::{Metadata, Trust};
pub use store::{StageStatus, UpdateSource, UpdateStore};
pub use verification::CandidateObservation;
#[cfg(test)]
mod tests;

pub fn target() -> Result<&'static str, String> {
    if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
        Ok("windows-x86_64")
    } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        Ok("linux-x86_64")
    } else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
        Ok("linux-aarch64")
    } else {
        Err("unsupported platform".into())
    }
}
