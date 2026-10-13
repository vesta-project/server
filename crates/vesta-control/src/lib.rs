//! Trusted, in-process canonical identity, policy and operations components.
//!
//! This crate does not authenticate a transport, expose a listener, enroll a device,
//! or turn Minecraft identity, passwords, OP, connector possession or loopback into
//! administrative authority. Production local authority is deliberately unavailable.
#![deny(unsafe_code)]

pub mod admission;
pub mod automation;
pub mod automation_host;
pub mod backups;
pub mod config;
pub mod content;
pub mod files;
pub mod host;
pub mod identity;
pub mod metrics;
pub mod minecraft_admission;
pub mod moderation;
pub mod operations;
pub mod pack_access;
pub mod packs;
pub mod recovery_export;
pub mod whitelist;

mod error;

pub use error::{ControlError, IdentityErrorCode};
pub type Result<T> = std::result::Result<T, ControlError>;
