//! Trusted in-process game ownership and bounded console capture, not authentication.
//!
//! Literal launch and generation-bound direct-child control are host-library components,
//! not an agent, independent supervisor, IPC, readiness or crash-continuity claim.
//! Console output can contain secrets: an application must enforce current console-read
//! policy before exposing snapshots. Task-view authority is not enough. Launch/commands
//! require trusted backend composition; these APIs do not authorize remote requests.
#![deny(unsafe_code)]

pub mod console;
pub mod launch_config;
pub mod players;
pub mod process;
