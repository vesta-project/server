//! Opt-in, OS-local startup discovery. Never downloads or activates executables.
use crate::store::{Result, unavailable};
use crate::{Metadata, UpdateSource, UpdateStore, metadata, release};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StartupPolicy {
    pub check_on_start: bool,
    /// Explicit operator-selected spacing, including failed/interrupted attempts.
    pub minimum_interval_ms: u64,
    pub source: UpdateSource,
}

impl StartupPolicy {
    fn validate_interval(&self) -> Result<()> {
        if self.minimum_interval_ms == 0 || self.minimum_interval_ms > i64::MAX as u64 {
            return Err("update_startup_policy_invalid".into());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckOutcome {
    Incomplete,
    Current,
    Available,
    Failed,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StartupCheck {
    pub attempted_at_ms: i64,
    pub outcome: CheckOutcome,
    pub candidate: Option<Metadata>,
    /// Stable allowlisted classification, never a raw URL or transport diagnostic.
    pub error_code: Option<String>,
}

impl UpdateStore {
    pub(super) fn startup_schema(&self) -> Result<()> {
        self.db.write(|c| {
            c.execute_batch("CREATE TABLE IF NOT EXISTS update_startup_policy(singleton INTEGER PRIMARY KEY CHECK(singleton=1),document TEXT NOT NULL CHECK(length(document)<=16384)) STRICT;
                CREATE TABLE IF NOT EXISTS update_startup_check(singleton INTEGER PRIMARY KEY CHECK(singleton=1),document TEXT NOT NULL CHECK(length(document)<=4096)) STRICT;")?;
            Ok(())
        }).map_err(unavailable)
    }

    pub fn startup_policy(&self) -> Result<Option<StartupPolicy>> {
        let json: Option<String> = self
            .db
            .read(|c| {
                Ok(c.query_row(
                    "SELECT document FROM update_startup_policy WHERE singleton=1",
                    [],
                    |r| r.get(0),
                )
                .optional()?)
            })
            .map_err(unavailable)?;
        json.map(|j| serde_json::from_str(&j).map_err(unavailable))
            .transpose()
    }

    /// Requires existing installation trust. Changing the source cannot change
    /// channel, release key or the accepted sequence. No default network source.
    pub fn configure_startup_check(&self, policy: &StartupPolicy) -> Result<()> {
        let _lock = self.lock()?;
        self.validate_startup_policy(policy)?;
        let json = serde_json::to_string(policy).map_err(unavailable)?;
        if json.len() > 16384 {
            return Err("update_startup_policy_invalid".into());
        }
        self.db.write(|c| {
            c.execute("INSERT INTO update_startup_policy(singleton,document) VALUES(1,?) ON CONFLICT(singleton) DO UPDATE SET document=excluded.document", [json])?;
            // A deliberate local reconfiguration permits an immediate fresh check.
            // The trust/anti-rollback record is untouched.
            c.execute("DELETE FROM update_startup_check", [])?;
            Ok(())
        }).map_err(unavailable)
    }

    fn validate_startup_policy(&self, policy: &StartupPolicy) -> Result<()> {
        let trust = self.trust()?;
        policy.validate_interval()?;
        match (&policy.source.local, &policy.source.base_url) {
            (Some(path), None) if path.is_absolute() => {
                if path
                    .components()
                    .any(|c| matches!(c, std::path::Component::ParentDir))
                {
                    return Err("update_startup_source_invalid".into());
                }
                if policy.check_on_start {
                    release::validate_local_directory(path)
                        .map_err(|_| "update_startup_source_invalid")?;
                }
            }
            (None, Some(url)) if url.len() <= 8192 => {
                release::validate_base_url(url).map_err(|_| "update_startup_source_invalid")?;
            }
            _ => return Err("update_startup_source_invalid".into()),
        }
        if policy
            .source
            .asset_prefix
            .as_deref()
            .is_some_and(|p| !p.is_empty() && p != format!("{}-", trust.target))
        {
            return Err("update_startup_source_invalid".into());
        }
        Ok(())
    }

    pub fn last_startup_check(&self) -> Result<Option<StartupCheck>> {
        let json: Option<String> = self
            .db
            .read(|c| {
                Ok(c.query_row(
                    "SELECT document FROM update_startup_check WHERE singleton=1",
                    [],
                    |r| r.get(0),
                )
                .optional()?)
            })
            .map_err(unavailable)?;
        json.map(|j| serde_json::from_str(&j).map_err(unavailable))
            .transpose()
    }

    fn record_startup_check(&self, check: &StartupCheck) -> Result<()> {
        let json = serde_json::to_string(check).map_err(unavailable)?;
        if json.len() > 4096 {
            return Err("update_state_unavailable".into());
        }
        self.db.write(|c| {
            c.execute("INSERT INTO update_startup_check(singleton,document) VALUES(1,?) ON CONFLICT(singleton) DO UPDATE SET document=excluded.document", params![json])?;
            Ok(())
        }).map_err(unavailable)
    }

    /// Returns None when disabled or spaced out. A successful observation is not
    /// execution authority. Check failures do not damage a healthy installation,
    /// advance trust or download binaries. Call off the service startup path.
    pub fn check_on_start(
        &self,
        installed_manifest: &str,
        now: i64,
    ) -> Result<Option<StartupCheck>> {
        if now < 0 || !metadata::valid_hash(installed_manifest) {
            return Err("update_startup_input_invalid".into());
        }
        let _lock = self.lock()?;
        let Some(policy) = self.startup_policy()? else {
            return Ok(None);
        };
        if !policy.check_on_start {
            return Ok(None);
        }
        // Validate persisted values as well as configuration input. Source access
        // itself may now fail (removed directory/offline network); record that
        // safely below, rather than silently claiming no update exists.
        policy.validate_interval()?;
        if let Some(last) = self.last_startup_check()? {
            if last.attempted_at_ms < 0 {
                return Err("update_state_unavailable".into());
            }
            if now < last.attempted_at_ms
                || (now - last.attempted_at_ms) < policy.minimum_interval_ms as i64
            {
                return Ok(None);
            }
        }
        let mut observation = StartupCheck {
            attempted_at_ms: now,
            outcome: CheckOutcome::Incomplete,
            candidate: None,
            error_code: None,
        };
        let installed_manifest = self
            .committed_manifest()?
            .unwrap_or_else(|| installed_manifest.to_owned());
        // Durable admission before I/O also spaces retries after interruption.
        self.record_startup_check(&observation)?;
        match self
            .validate_startup_policy(&policy)
            .and_then(|()| self.check(&policy.source, now))
        {
            Ok(m) => {
                observation.outcome = if m.manifest_sha256.eq_ignore_ascii_case(&installed_manifest)
                {
                    CheckOutcome::Current
                } else {
                    CheckOutcome::Available
                };
                observation.candidate = Some(m);
            }
            Err(error) => {
                observation.outcome = CheckOutcome::Failed;
                observation.error_code = Some(match error.as_str() {
                    "update_metadata_expired"
                    | "update_clock_rollback"
                    | "update_rollback_refused"
                    | "update_signature_invalid"
                    | "update_metadata_invalid"
                    | "update_trust_invalid"
                    | "update_trust_not_initialized"
                    | "update_startup_source_invalid" => error,
                    _ => "update_check_unavailable".into(),
                });
            }
        }
        self.record_startup_check(&observation)?;
        Ok(Some(observation))
    }
}
