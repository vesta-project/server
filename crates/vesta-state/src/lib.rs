//! Durable, local SQLite state foundation. This is not a production Java/Rust shared-writer
//! protocol. Migrate and inspect copies until runtime ownership is explicitly transferred.
//!
//! Owner-only modes/ACLs and path checks are same-account hygiene, not confinement. A process
//! running as the service account can still race path checks or access the state; the runner
//! lock is advisory and there is no independent supervisor/security boundary here.
#![deny(unsafe_code)]

pub mod artifacts;
pub mod audit;
pub mod events;
pub mod export;
pub mod settings;

mod clock;
mod database;
mod directory;
mod error;
mod migrations;
mod values;
#[cfg(windows)]
#[allow(unsafe_code)]
mod windows_security;

pub use clock::{Clock, SystemClock};
pub use database::{
    BUSY_TIMEOUT_MS, CURRENT_SCHEMA_VERSION, ReadTransaction, RuntimeLease, StateDatabase,
    WriteTransaction,
};
pub use directory::{RunnerLock, StateDirectory};
pub use error::{StateError, TransactionError};

pub type Result<T> = std::result::Result<T, StateError>;

#[cfg(feature = "latency-probe")]
pub mod latency_probe;

pub const UPDATE_ACTIVATION_PROTOCOL: u16 = 1;

/// Read-only compiled protocol declaration shared by release components. No state is opened.
pub fn update_capabilities(supervisor_ipc: u8) -> serde_json::Value {
    let target = if cfg!(all(windows, target_arch = "x86_64")) {
        "windows-x86_64"
    } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        "linux-x86_64"
    } else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
        "linux-aarch64"
    } else {
        "unsupported"
    };
    serde_json::json!({"target":target,"activation":{"protocol":UPDATE_ACTIVATION_PROTOCOL,"state_schema_min":CURRENT_SCHEMA_VERSION,"state_schema_max":CURRENT_SCHEMA_VERSION,"supervisor_ipc":supervisor_ipc}})
}
