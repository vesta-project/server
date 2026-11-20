//! Host-owned persistence and authority. These are trusted in-process APIs:
//! account ids and local-recovery actors must come from authenticated adapters,
//! never directly from request bodies. Workers cannot mint these authorities.
pub mod owners;
pub mod registry;
pub mod removed;
pub mod state;
pub mod storage;

pub mod accounts;
pub mod claim;
pub mod creates;
pub mod imports;
pub mod invites;
pub mod link;
pub mod recovery;
pub mod resources;
pub mod sessions;
pub mod settings;
pub mod signin;

pub use state::{HostSnapshot, HostState};

#[cfg(test)]
mod enrollment_tests;
#[cfg(test)]
mod signin_tests;
#[cfg(test)]
mod tests;
