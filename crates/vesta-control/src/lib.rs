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
pub mod external_alerts;
pub mod files;
pub mod host;
mod host_software;
pub mod identity;
pub mod java;
pub mod metrics;
pub mod minecraft_admission;
pub mod moderation;
pub mod motd;
pub mod onboarding;
pub mod operations;
pub mod pack_access;
pub mod packs;
pub mod recovery_export;
pub mod resource_packs;
pub mod sftp;
pub mod sleep;
pub mod software;
pub mod whitelist;
pub mod world;

mod error;

pub use error::{ControlError, IdentityErrorCode};
pub type Result<T> = std::result::Result<T, ControlError>;

#[cfg(feature = "latency-probe")]
pub use vesta_state::latency_probe;
