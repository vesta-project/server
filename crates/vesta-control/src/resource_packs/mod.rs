//! Trusted composition for ordered, disk-backed Minecraft resource-pack builds.
//!
//! The builder accepts pinned open inputs and immutable protected artifacts.
//! The separate `hosting` service adds canonical owner authority, durable drafts,
//! jobs, reviewed settings activation and explicit public-release registration.
mod archive;
mod build;
pub mod hosting;
mod model;
mod semantic;

pub use build::{BuiltPack, PublishedPack, build};
pub use model::{BuildLimits, BuildReport, Error, Input, PackFormat, Resolution, ResolvedPath};
pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests;
