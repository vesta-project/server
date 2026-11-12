//! Revisioned host fallback / stored telemetry policy. Reads are cheap and happen
//! on every sampler tick, including when disabled, so changes need no restart.
use crate::config::MetricsSettings;
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use vesta_state::{Result, StateError};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct CommandPolling {
    pub enabled: bool,
    pub interval_seconds: u64,
}
impl Default for CommandPolling {
    fn default() -> Self {
        Self {
            enabled: false,
            interval_seconds: 60,
        }
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Document {
    pub settings: MetricsSettings,
    #[serde(default)]
    pub command_polling: CommandPolling,
}
impl Document {
    pub fn host(settings: MetricsSettings) -> Self {
        Self {
            settings,
            command_polling: CommandPolling::default(),
        }
    }
    pub fn valid(self) -> bool {
        self.settings.validated().is_ok()
            && (30..=3600).contains(&self.command_polling.interval_seconds)
    }
}
#[derive(Serialize)]
pub struct Policy {
    pub source: &'static str,
    pub revision: Option<i64>,
    #[serde(flatten)]
    pub document: Document,
    pub storage_bytes: i64,
    /// Explicit consent warning; this is not a silent/zero-log tick source.
    pub command_polling_writes_console: bool,
}
pub(crate) fn effective(
    c: &Connection,
    fallback: MetricsSettings,
) -> Result<(Option<i64>, Document)> {
    let stored: Option<(i64, String)> = c
        .query_row(
            "SELECT revision,document FROM metrics_policy WHERE singleton=1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    match stored {
        None => Ok((None, Document::host(fallback))),
        Some((revision, json)) => {
            let document: Document = serde_json::from_str(&json)
                .map_err(|_| StateError::new("state_metrics_unavailable"))?;
            if !document.valid() || revision <= 0 {
                return Err(StateError::new("state_metrics_unavailable"));
            }
            Ok((Some(revision), document))
        }
    }
}
pub(crate) fn get(c: &Connection, fallback: MetricsSettings) -> Result<Policy> {
    let (revision, document) = effective(c, fallback)?;
    let storage_bytes = c.query_row(
        "SELECT storage_bytes FROM metrics_status WHERE singleton=1",
        [],
        |r| r.get(0),
    )?;
    Ok(Policy {
        source: if revision.is_some() {
            "stored"
        } else {
            "host_config"
        },
        revision,
        document,
        storage_bytes,
        command_polling_writes_console: true,
    })
}
