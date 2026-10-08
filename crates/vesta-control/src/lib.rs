//! Trusted, in-process canonical identity, policy and operations components.
//!
//! This crate does not authenticate a transport, expose a listener, enroll a device,
//! or turn Minecraft identity, passwords, OP, connector possession or loopback into
//! administrative authority. Production local authority is deliberately unavailable.
#![deny(unsafe_code)]

pub mod identity;
pub mod operations;

mod error;

pub use error::{ControlError, IdentityErrorCode};
pub type Result<T> = std::result::Result<T, ControlError>;
