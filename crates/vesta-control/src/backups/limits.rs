//! Backup budgets (F25): restore-point counts, repository size and free disk space.
//!
//! Reaching a budget stops new backups with a fixed error code; nothing is ever deleted
//! to make room. Counts are judged after the profile's retention, so a profile whose
//! retention already bounds it is never stopped by an equal cap. The repository size is
//! checked before a backup and enforced while it writes: past its budget the backup
//! stops, and the chunks it stored are pruned later as unreferenced data. Safety
//! snapshots taken before a restore only respect the free-space reserve.
use super::{
    BackupError, Backups, Inner, JobState, Result, engine, profile_tag, restore::safety_tag, space,
};
use crate::config::{BackupLimits, BackupProfile, BackupRepository};
use engine::{EngineError, Retention};
use rusqlite::OptionalExtension;

pub const SNAPSHOT_LIMIT: &str = "backup_snapshot_limit_reached";
pub const STORAGE_LIMIT: &str = "backup_storage_limit_reached";
pub const DISK_SPACE_LOW: &str = "backup_disk_space_low";
const LIMIT_CODES: [&str; 3] = [SNAPSHOT_LIMIT, STORAGE_LIMIT, DISK_SPACE_LOW];

/// How many new bytes a backup may store, and which budget binds.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) struct Budget {
    pub bytes: Option<u64>,
    /// The repository disk's free-space reserve, not the size limit, is the bound.
    pub disk_bound: bool,
}
impl Budget {
    /// The fixed code for a backup that ran past this budget.
    pub fn exceeded(self) -> &'static str {
        if self.disk_bound {
            DISK_SPACE_LOW
        } else {
            STORAGE_LIMIT
        }
    }
}

/// Repository usage against the configured budgets, for status displays.
#[derive(Clone, Debug, PartialEq)]
pub struct StorageStatus {
    pub repository_bytes: u64,
    /// Accepted restore points across all profiles (safety snapshots excluded).
    pub restore_points: usize,
    pub safety_snapshots: usize,
    /// Free space on a local repository's disk (`None` for S3 or when unknown).
    pub free_bytes: Option<u64>,
    pub limits: BackupLimits,
    /// Set when the latest backup was refused by a budget: backups stay stopped until
    /// the owner raises the limit, tightens retention or frees space.
    pub backups_stopped: Option<String>,
}

impl Inner {
    /// Free-space budget on a local repository's disk (`None` for S3).
    pub(super) fn disk_budget(&self) -> std::result::Result<Budget, &'static str> {
        let BackupRepository::Local(path) = &self.settings.repository else {
            return Ok(Budget::default());
        };
        let free = space::available(path).ok_or(DISK_SPACE_LOW)?;
        let reserve = self.policy().limits.min_free_bytes;
        if free <= reserve {
            return Err(DISK_SPACE_LOW);
        }
        Ok(Budget {
            bytes: Some(free - reserve),
            disk_bound: true,
        })
    }

    /// Check every budget before a profile backup; the result bounds what it may store.
    pub(super) fn capacity(
        &self,
        profile: &BackupProfile,
    ) -> std::result::Result<Budget, &'static str> {
        let policy = self.policy();
        let limits = &policy.limits;
        if profile.max_snapshots.is_some() || limits.max_snapshots.is_some() {
            let retention = Retention {
                keep_total: profile.keep_total,
                keep_hourly: profile.keep_hourly,
                keep_last: profile.keep_last,
                keep_daily: profile.keep_daily,
                keep_weekly: profile.keep_weekly,
                keep_monthly: profile.keep_monthly,
            };
            let tag = profile_tag(&profile.name);
            let after = self
                .engine
                .retained_after_backup(&tag, retention)
                .map_err(EngineError::code)?;
            if profile
                .max_snapshots
                .is_some_and(|max| after > max as usize)
            {
                return Err(SNAPSHOT_LIMIT);
            }
            if let Some(max) = limits.max_snapshots {
                let counts = self.engine.tag_counts().map_err(EngineError::code)?;
                let others: usize = policy
                    .profiles
                    .iter()
                    .filter(|p| p.name != profile.name)
                    .map(|p| counts.get(&profile_tag(&p.name)).copied().unwrap_or(0))
                    .sum();
                if after + others > max as usize {
                    return Err(SNAPSHOT_LIMIT);
                }
            }
        }
        let mut budget = Budget::default();
        if let Some(max) = limits.max_repository_bytes {
            let used = self.engine.usage().map_err(EngineError::code)?;
            if used >= max {
                return Err(STORAGE_LIMIT);
            }
            budget.bytes = Some(max - used);
        }
        let disk = self.disk_budget()?;
        if let Some(free) = disk.bytes
            && budget.bytes.is_none_or(|b| free < b)
        {
            budget = disk;
        }
        Ok(budget)
    }
}

impl Backups {
    /// Repository usage and budgets. Bounded like snapshot listing: one at a time.
    pub fn storage(&self) -> Result<StorageStatus> {
        let inner = &self.inner;
        let _single = inner.listing.try_lock().map_err(|_| BackupError::Busy)?;
        let unavailable = |error: EngineError| match error {
            EngineError::Unavailable => BackupError::Busy,
            _ => BackupError::Unavailable,
        };
        let repository_bytes = inner.engine.usage().map_err(unavailable)?;
        let counts = inner.engine.tag_counts().map_err(unavailable)?;
        let count = |tag: String| counts.get(&tag).copied().unwrap_or(0);
        let policy = inner.policy();
        let profiles = &policy.profiles;
        let restore_points = profiles.iter().map(|p| count(profile_tag(&p.name))).sum();
        let safety_snapshots = profiles.iter().map(|p| count(safety_tag(&p.name))).sum();
        let free_bytes = match &inner.settings.repository {
            BackupRepository::Local(path) => space::available(path),
            BackupRepository::S3(_) => None,
        };
        let latest: Option<(String, Option<String>)> = inner.db.read(|c| {
            Ok(c.query_row(
                "SELECT state,error_code FROM backup_jobs WHERE kind='backup' AND state IN ('succeeded','failed','uncertain') ORDER BY finished_at DESC, id DESC LIMIT 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?)
        })?;
        let backups_stopped = latest.and_then(|(state, code)| {
            (state == JobState::Failed.wire())
                .then_some(code)
                .flatten()
                .filter(|code| LIMIT_CODES.contains(&code.as_str()))
        });
        Ok(StorageStatus {
            repository_bytes,
            restore_points,
            safety_snapshots,
            free_bytes,
            limits: policy.limits.clone(),
            backups_stopped,
        })
    }
}
