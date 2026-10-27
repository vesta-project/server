//! Trusted composition for ordered, disk-backed Minecraft resource-pack builds.
//!
//! This module does not authorize a caller, fetch URLs, expose downloads, edit
//! Minecraft configuration or activate a release. Inputs are pinned open files;
//! artifacts reuse protected state storage and never replace existing bytes.
mod archive;
mod build;
mod model;
mod semantic;

pub use build::{BuiltPack, PublishedPack, build};
pub use model::{BuildLimits, BuildReport, Error, Input, PackFormat, Resolution, ResolvedPath};
pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests;
