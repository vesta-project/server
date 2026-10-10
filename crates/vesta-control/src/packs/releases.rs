//! Monotonic hosted-pack releases bound to content-addressed artifacts.
//!
//! Each publication appends one immutable row; the highest revision is the hosted
//! release. Publishing requires `pack.publish`; raising the minimum revision also
//! requires `pack.force_update`. Authority and the expected current revision are
//! rechecked in the same write transaction that records the release and its audit.
//! A release is a hosting record, not a download grant, session or pack-access decision.
use super::{
    BuiltVestaPack, PackArtifacts, PackError, PreparedArchive, PruneReport, PublishedArtifact,
};
use crate::ControlError;
use crate::config::PackMetadata;
use crate::identity::{self, Capability, Decision, EpochMillis, Subject};
use rusqlite::{Connection, OptionalExtension, params};
use std::{
    cmp::Ordering,
    collections::HashMap,
    fmt,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock, Weak},
};
use tokio::sync::watch;
use vesta_protocol::pack::catalogue::{
    CatalogueError, MAX_REVISION, MAX_TEXT_BYTES, ManifestMetadata, Sha256,
};
use vesta_state::audit::{AuditActor, AuditLog, AuditRecord};
use vesta_state::{StateDatabase, StateError, TransactionError};

use super::publish::ArtifactFormat;

const SOURCE: &str = "pack-release";
const TARGET: &str = "pack-release";
const PUBLISH: &str = "pack_release.publish";
const PRUNE: &str = "pack_release.prune";

#[derive(Clone, Eq, PartialEq)]
pub enum ReleaseError {
    /// The expected current revision no longer matches.
    Conflict,
    RevisionExhausted,
    /// The built pack does not embed the planned revision and minimum revision.
    RevisionMismatch,
    /// The display version is lower than, or not comparable with, the hosted one.
    VersionNotIncreasing,
    InvalidVersion,
    Pack(PackError),
    Control(ControlError),
    State(StateError),
    Rollback {
        primary: Box<ReleaseError>,
        failure: StateError,
    },
}
impl ReleaseError {
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Conflict => "pack_release_conflict",
            Self::RevisionExhausted => "pack_release_revision_exhausted",
            Self::RevisionMismatch => "pack_release_revision_mismatch",
            Self::VersionNotIncreasing => "pack_release_version_not_increasing",
            Self::InvalidVersion => "pack_release_version_invalid",
            Self::Pack(error) => error.code(),
            Self::Control(error) => error.code(),
            Self::State(error) => error.code(),
            Self::Rollback { .. } => "rollback_failed",
        }
    }
}
impl fmt::Display for ReleaseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.code())
    }
}
impl fmt::Debug for ReleaseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ReleaseError")
            .field("code", &self.code())
            .finish_non_exhaustive()
    }
}
impl std::error::Error for ReleaseError {}
impl From<PackError> for ReleaseError {
    fn from(error: PackError) -> Self {
        Self::Pack(error)
    }
}
impl From<ControlError> for ReleaseError {
    fn from(error: ControlError) -> Self {
        Self::Control(error)
    }
}
impl From<StateError> for ReleaseError {
    fn from(error: StateError) -> Self {
        Self::State(error)
    }
}
impl From<rusqlite::Error> for ReleaseError {
    fn from(error: rusqlite::Error) -> Self {
        Self::State(error.into())
    }
}
impl From<TransactionError<ReleaseError>> for ReleaseError {
    fn from(error: TransactionError<ReleaseError>) -> Self {
        match error {
            TransactionError::State(error) => Self::State(error),
            TransactionError::Work(error) => error,
            TransactionError::Rollback { primary, failure } => Self::Rollback {
                primary: Box::new(Self::from(*primary)),
                failure,
            },
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UpdatePolicy {
    /// Keep the current minimum revision: installed still-allowed revisions may join.
    Optional,
    /// Raise the minimum revision to the new release.
    Required,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UpdateStatus {
    Current,
    UpdateAvailable,
    UpdateRequired,
}
impl UpdateStatus {
    pub const fn wire(self) -> &'static str {
        match self {
            Self::Current => "current",
            Self::UpdateAvailable => "update_available",
            Self::UpdateRequired => "update_required",
        }
    }
}

/// The hosted release. Revisions are compatibility signals, not a security boundary.
#[derive(Clone, Eq, PartialEq)]
pub struct PackRelease {
    revision: u64,
    minimum_revision: u64,
    display_version: String,
    artifact: PublishedArtifact,
    published_at: EpochMillis,
}
impl PackRelease {
    pub const fn revision(&self) -> u64 {
        self.revision
    }
    pub const fn minimum_revision(&self) -> u64 {
        self.minimum_revision
    }
    pub fn display_version(&self) -> &str {
        &self.display_version
    }
    pub fn artifact(&self) -> &PublishedArtifact {
        &self.artifact
    }
    pub const fn published_at(&self) -> EpochMillis {
        self.published_at
    }
    /// Revision 0 means no installation. A claim above the hosted revision never came
    /// from this ledger (revisions only increase), so it must move to the hosted one.
    pub const fn status_for(&self, installed: u64) -> UpdateStatus {
        if installed == self.revision {
            UpdateStatus::Current
        } else if installed >= self.minimum_revision && installed < self.revision {
            UpdateStatus::UpdateAvailable
        } else {
            UpdateStatus::UpdateRequired
        }
    }
}
impl fmt::Debug for PackRelease {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PackRelease")
            .field("revision", &self.revision)
            .field("minimum_revision", &self.minimum_revision)
            .field("artifact", &self.artifact)
            .finish_non_exhaustive()
    }
}

/// Revisions reserved for one publication. Not authority: publication rechecks both.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReleasePlan {
    previous: u64,
    revision: u64,
    minimum_revision: u64,
    policy: UpdatePolicy,
}
impl ReleasePlan {
    pub const fn revision(&self) -> u64 {
        self.revision
    }
    pub const fn minimum_revision(&self) -> u64 {
        self.minimum_revision
    }
    /// Snapshot metadata carrying the planned revisions for `PackPreparation::snapshot`.
    pub fn manifest_metadata(&self, base: &PackMetadata) -> Result<ManifestMetadata, ReleaseError> {
        ManifestMetadata::new(
            base.name(),
            base.version(),
            self.revision,
            self.minimum_revision,
        )
        .map_err(|error| PackError::Catalogue(error).into())
    }
    /// Builder metadata carrying the planned revisions for `build_vesta`.
    pub fn pack_metadata(&self, base: &PackMetadata) -> PackMetadata {
        base.with_revisions(self.revision, self.minimum_revision)
    }
}

/// Trusted local composition over the canonical state database.
pub struct PackReleases {
    db: StateDatabase,
    audit: AuditLog,
    artifacts: PackArtifacts,
    changes: Arc<watch::Sender<u64>>,
}
impl fmt::Debug for PackReleases {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PackReleases[<redacted>]")
    }
}
impl PackReleases {
    pub fn open(db: &StateDatabase) -> Result<Self, ReleaseError> {
        Ok(Self {
            audit: AuditLog::new(db.clone())?,
            artifacts: PackArtifacts::open(db)?,
            db: db.clone(),
            changes: release_notifications(db)?,
        })
    }

    /// Latest committed revision signal, shared by local stores for this database.
    /// Subscribe before reading `current` to avoid losing a concurrent publication.
    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.changes.subscribe()
    }

    pub fn artifacts(&self) -> &PackArtifacts {
        &self.artifacts
    }

    /// The hosted release for trusted serving code. Pack-access policy is the caller's.
    pub fn current(&self) -> Result<Option<PackRelease>, ReleaseError> {
        Ok(self
            .db
            .read_with(|snapshot| current(snapshot.connection(), self.db.server_id()))?)
    }

    /// Reserve the next revision against `expected` (0 when nothing is published yet).
    pub fn plan(
        &self,
        subject: &Subject,
        expected: u64,
        policy: UpdatePolicy,
    ) -> Result<ReleasePlan, ReleaseError> {
        let result = self.db.read_with(|snapshot| {
            let connection = snapshot.connection();
            let current = current(connection, self.db.server_id())?;
            let plan = plan_from(current.as_ref(), expected, policy)?;
            self.authorize(connection, subject, plan.raises_minimum())?;
            Ok(plan)
        });
        self.audit_refusal(subject, PUBLISH, result.map_err(ReleaseError::from))
    }

    pub fn publish_vesta(
        &self,
        subject: &Subject,
        plan: &ReleasePlan,
        pack: &BuiltVestaPack,
        acknowledge_lower_version: bool,
    ) -> Result<PackRelease, ReleaseError> {
        if pack.revision() != plan.revision || pack.minimum_revision() != plan.minimum_revision {
            return Err(ReleaseError::RevisionMismatch);
        }
        self.preflight(subject, plan, pack.version(), acknowledge_lower_version)?;
        let (artifact, created) = self.artifacts.publish_vesta_tracked(pack)?;
        self.record_publication(
            subject,
            plan,
            (artifact, created),
            pack.version(),
            acknowledge_lower_version,
        )
    }

    /// Opaque `.zip`/`.mrpack` archives do not embed a revision; the ledger supplies it.
    pub fn publish_archive(
        &self,
        subject: &Subject,
        plan: &ReleasePlan,
        archive: &PreparedArchive,
        display_version: &str,
        acknowledge_lower_version: bool,
    ) -> Result<PackRelease, ReleaseError> {
        self.preflight(subject, plan, display_version, acknowledge_lower_version)?;
        let (artifact, created) = self.artifacts.publish_archive_tracked(archive)?;
        self.record_publication(
            subject,
            plan,
            (artifact, created),
            display_version,
            acknowledge_lower_version,
        )
    }

    /// Publish a configured immutable preparation with the same transaction-time
    /// capability, revision, force-update and display-version checks as other modes.
    pub fn publish_prepared(
        &self,
        subject: &Subject,
        plan: &ReleasePlan,
        pack: &super::PreparedPack,
        acknowledge_lower_version: bool,
    ) -> Result<PackRelease, ReleaseError> {
        let metadata = pack.manifest.metadata();
        if metadata.revision() != plan.revision
            || metadata.minimum_revision() != plan.minimum_revision
        {
            return Err(ReleaseError::RevisionMismatch);
        }
        match &pack.content {
            super::configured::PreparedContent::Scanned(built) => {
                self.publish_vesta(subject, plan, built, acknowledge_lower_version)
            }
            super::configured::PreparedContent::Archive(archive) => self.publish_archive(
                subject,
                plan,
                archive,
                metadata.version(),
                acknowledge_lower_version,
            ),
            super::configured::PreparedContent::Platform { bytes, hash } => {
                self.preflight(subject, plan, metadata.version(), acknowledge_lower_version)?;
                let published = self.artifacts.publish_platform_tracked(bytes, hash)?;
                self.record_publication(
                    subject,
                    plan,
                    published,
                    metadata.version(),
                    acknowledge_lower_version,
                )
            }
        }
    }

    fn preflight(
        &self,
        subject: &Subject,
        plan: &ReleasePlan,
        display_version: &str,
        acknowledge_lower_version: bool,
    ) -> Result<(), ReleaseError> {
        validate_version(display_version)?;
        let result = self.db.read_with(|snapshot| {
            self.check_publication(
                snapshot.connection(),
                subject,
                plan,
                display_version,
                acknowledge_lower_version,
            )?;
            Ok(())
        });
        self.audit_refusal(subject, PUBLISH, result.map_err(ReleaseError::from))
    }

    fn check_publication(
        &self,
        connection: &Connection,
        subject: &Subject,
        plan: &ReleasePlan,
        display_version: &str,
        acknowledge_lower_version: bool,
    ) -> Result<EpochMillis, ReleaseError> {
        let now = self.authorize(connection, subject, plan.raises_minimum())?;
        let current = current(connection, self.db.server_id())?;
        if plan_from(current.as_ref(), plan.previous, plan.policy)? != *plan {
            return Err(ReleaseError::Conflict);
        }
        if let Some(current) = &current
            && !acknowledge_lower_version
            && !version_not_lower(display_version, &current.display_version)
        {
            return Err(ReleaseError::VersionNotIncreasing);
        }
        Ok(now)
    }

    fn record_publication(
        &self,
        subject: &Subject,
        plan: &ReleasePlan,
        publication: (PublishedArtifact, bool),
        display_version: &str,
        acknowledge_lower_version: bool,
    ) -> Result<PackRelease, ReleaseError> {
        let (artifact, created) = publication;
        let result = self.record(
            subject,
            plan,
            artifact.clone(),
            display_version,
            acknowledge_lower_version,
        );
        if created && let Err(primary) = result {
            // The immediate transaction excludes successful recording while checking
            // every historical release reference, including a concurrent winner's.
            let cleanup = self.db.write_with(|transaction| {
                let referenced: bool = transaction.connection().query_row(
                    "SELECT EXISTS(SELECT 1 FROM pack_releases WHERE format=? AND sha256=?)",
                    params![artifact.format().extension(), artifact.sha256().to_hex()],
                    |row| row.get(0),
                )?;
                if !referenced {
                    self.artifacts.remove_unreferenced(&artifact)?;
                }
                Ok::<_, ReleaseError>(())
            });
            return match cleanup {
                Ok(()) => Err(primary),
                Err(_) => Err(ReleaseError::Rollback {
                    primary: Box::new(primary),
                    failure: StateError::new("pack_release_artifact_cleanup_failed"),
                }),
            };
        }
        result
    }

    fn record(
        &self,
        subject: &Subject,
        plan: &ReleasePlan,
        artifact: PublishedArtifact,
        display_version: &str,
        acknowledge_lower_version: bool,
    ) -> Result<PackRelease, ReleaseError> {
        validate_version(display_version)?;
        let result = self.db.write_with(|transaction| {
            let connection = transaction.connection();
            let server = self.db.server_id();
            let now = self.check_publication(connection, subject, plan, display_version, acknowledge_lower_version)?;
            // Publication was verified before this transaction; a concurrent prune or
            // tamper since then must not leave the hosted release pointing at nothing.
            self.artifacts.present(&artifact)?;
            connection.execute(
                "INSERT INTO pack_releases (server_id,revision,minimum_revision,display_version,format,sha256,size,published_at) VALUES (?,?,?,?,?,?,?,?)",
                params![
                    server,
                    to_sql(plan.revision)?,
                    to_sql(plan.minimum_revision)?,
                    display_version,
                    artifact.format().extension(),
                    artifact.sha256().to_hex(),
                    to_sql(artifact.size())?,
                    now,
                ],
            )?;
            self.audit.append(
                connection,
                &AuditRecord::new(
                    actor(subject)?,
                    PUBLISH.to_owned(),
                    TARGET.to_owned(),
                    "success".to_owned(),
                    None,
                    [
                        ("revision".to_owned(), Some(plan.revision.to_string())),
                        (
                            "minimum_revision".to_owned(),
                            Some(plan.minimum_revision.to_string()),
                        ),
                        ("previous_revision".to_owned(), Some(plan.previous.to_string())),
                        (
                            "format".to_owned(),
                            Some(artifact.format().extension().to_owned()),
                        ),
                        ("size".to_owned(), Some(artifact.size().to_string())),
                    ],
                )?,
            )?;
            Ok(PackRelease {
                revision: plan.revision,
                minimum_revision: plan.minimum_revision,
                display_version: display_version.to_owned(),
                artifact: artifact.clone(),
                published_at: now,
            })
        });
        let release = self.audit_refusal(subject, PUBLISH, result.map_err(ReleaseError::from))?;
        // Publish only after commit. Concurrent completions cannot regress the signal.
        self.changes.send_if_modified(|revision| {
            if *revision < release.revision {
                *revision = release.revision;
                true
            } else {
                false
            }
        });
        Ok(release)
    }

    /// Remove published artifacts other than the hosted release's, plus abandoned stages.
    /// Runs inside a write transaction so no release can be recorded concurrently; an
    /// unrecorded publication that loses its artifact here fails its own recording safely.
    pub fn prune(&self, subject: &Subject) -> Result<PruneReport, ReleaseError> {
        let result = self.db.write_with(|transaction| {
            let connection = transaction.connection();
            self.authorize(connection, subject, false)?;
            let current = current(connection, self.db.server_id())?;
            let report = self
                .artifacts
                .prune_except(current.as_ref().map(PackRelease::artifact))?;
            self.audit.append(
                connection,
                &AuditRecord::new(
                    actor(subject)?,
                    PRUNE.to_owned(),
                    TARGET.to_owned(),
                    "success".to_owned(),
                    None,
                    [
                        ("removed".to_owned(), Some(report.removed.to_string())),
                        ("retained".to_owned(), Some(report.retained.to_string())),
                    ],
                )?,
            )?;
            Ok(report)
        });
        self.audit_refusal(subject, PRUNE, result.map_err(ReleaseError::from))
    }

    fn authorize(
        &self,
        connection: &Connection,
        subject: &Subject,
        raises_minimum: bool,
    ) -> Result<EpochMillis, ReleaseError> {
        let now = self.db.clock().millis();
        let policy = identity::policy_in(connection, self.db.server_id(), now, Some(subject))?;
        let mut required = vec![Capability::PackPublish];
        if raises_minimum {
            required.push(Capability::PackForceUpdate);
        }
        for capability in required {
            if let Decision::Deny(reason) = policy.check(capability, None) {
                return Err(ControlError::forbidden(reason).into());
            }
        }
        Ok(now)
    }

    /// Authority refusals are audited after the clean rollback; other errors are not.
    fn audit_refusal<T>(
        &self,
        subject: &Subject,
        action: &str,
        result: Result<T, ReleaseError>,
    ) -> Result<T, ReleaseError> {
        let Err(ReleaseError::Control(primary)) = &result else {
            return result;
        };
        let Some(reason) = primary.reason() else {
            return result;
        };
        let audit = actor(subject)
            .and_then(|actor| {
                AuditRecord::new(
                    actor,
                    action.to_owned(),
                    TARGET.to_owned(),
                    "denied".to_owned(),
                    None,
                    [("reason".to_owned(), Some(reason.code().to_owned()))],
                )
            })
            .and_then(|record| self.audit.append_record(&record));
        match audit {
            Ok(_) => result,
            Err(error) => Err(ControlError::AuditUnavailable {
                primary: Box::new(primary.clone()),
                audit_failure: Box::new(ControlError::State(error)),
            }
            .into()),
        }
    }
}

impl ReleasePlan {
    const fn raises_minimum(&self) -> bool {
        matches!(self.policy, UpdatePolicy::Required) && self.previous != 0
    }
}

fn plan_from(
    current: Option<&PackRelease>,
    expected: u64,
    policy: UpdatePolicy,
) -> Result<ReleasePlan, ReleaseError> {
    if current.map_or(0, PackRelease::revision) != expected {
        return Err(ReleaseError::Conflict);
    }
    let revision = expected
        .checked_add(1)
        .filter(|revision| *revision <= MAX_REVISION)
        .ok_or(ReleaseError::RevisionExhausted)?;
    // The first release has no installed clients to keep compatible.
    let minimum_revision = match (current, policy) {
        (Some(current), UpdatePolicy::Optional) => current.minimum_revision,
        _ => revision,
    };
    Ok(ReleasePlan {
        previous: expected,
        revision,
        minimum_revision,
        policy,
    })
}

fn current(connection: &Connection, server: &str) -> Result<Option<PackRelease>, ReleaseError> {
    let row = connection
        .query_row(
            "SELECT revision,minimum_revision,display_version,format,sha256,size,published_at FROM pack_releases WHERE server_id=? ORDER BY revision DESC LIMIT 1",
            [server],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, i64>(6)?,
                ))
            },
        )
        .optional()?;
    let Some((revision, minimum, version, format, sha256, size, published_at)) = row else {
        return Ok(None);
    };
    let format = match format.as_str() {
        "vestapack" => ArtifactFormat::Vesta,
        "zip" => ArtifactFormat::Zip,
        "mrpack" => ArtifactFormat::Mrpack,
        "catalogue" => ArtifactFormat::Catalogue,
        _ => return Err(corrupt()),
    };
    let revision = u64::try_from(revision).map_err(|_| corrupt())?;
    let minimum_revision = u64::try_from(minimum).map_err(|_| corrupt())?;
    if revision == 0 || minimum_revision == 0 || minimum_revision > revision {
        return Err(corrupt());
    }
    validate_version(&version).map_err(|_| corrupt())?;
    Ok(Some(PackRelease {
        revision,
        minimum_revision,
        display_version: version,
        artifact: PublishedArtifact::from_parts(
            format,
            Sha256::parse(&sha256).map_err(|_: CatalogueError| corrupt())?,
            u64::try_from(size).map_err(|_| corrupt())?,
        ),
        published_at,
    }))
}

fn validate_version(version: &str) -> Result<(), ReleaseError> {
    if version.is_empty()
        || version.len() > MAX_TEXT_BYTES
        || version.trim() != version
        || version.chars().any(char::is_control)
    {
        return Err(ReleaseError::InvalidVersion);
    }
    Ok(())
}

/// Numeric dotted versions compare by value (`1.10 > 1.9`, `1.0 == 1.0.0`); any other
/// spelling is only "not lower" when identical, so the owner must acknowledge it.
fn version_not_lower(next: &str, current: &str) -> bool {
    match (dotted(next), dotted(current)) {
        (Some(next), Some(current)) => {
            let length = next.len().max(current.len());
            let part = |parts: &[u64], index: usize| parts.get(index).copied().unwrap_or(0);
            (0..length)
                .map(|index| part(&next, index).cmp(&part(&current, index)))
                .find(|ordering| *ordering != Ordering::Equal)
                .unwrap_or(Ordering::Equal)
                != Ordering::Less
        }
        _ => next == current,
    }
}

fn dotted(version: &str) -> Option<Vec<u64>> {
    version
        .split('.')
        .map(|part| {
            (!part.is_empty() && part.len() <= 18 && part.bytes().all(|b| b.is_ascii_digit()))
                .then(|| part.parse().ok())
                .flatten()
        })
        .collect()
}

fn to_sql(value: u64) -> Result<i64, ReleaseError> {
    i64::try_from(value).map_err(|_| ReleaseError::RevisionExhausted)
}

fn actor(subject: &Subject) -> vesta_state::Result<AuditActor> {
    AuditActor::new(
        subject.principal.as_str().to_owned(),
        subject.device.as_str().to_owned(),
        SOURCE.to_owned(),
    )
}

fn corrupt() -> ReleaseError {
    StateError::new("state_pack_release_invalid").into()
}

#[cfg(test)]
#[path = "releases_tests.rs"]
mod tests;

// Weak registry shares notifications even when trusted local composition opens a
// second PackReleases handle. Entries die with their stores; no release queue is kept.
// Keyed by the canonical directory (as file mutation admission is) so alternative
// spellings of one database share a signal. Other processes are not covered here;
// realtime sockets also compare the hosted release on every heartbeat.
fn release_notifications(db: &StateDatabase) -> Result<Arc<watch::Sender<u64>>, ReleaseError> {
    notification_sender(db.directory().path())
}

fn notification_sender(directory: &Path) -> Result<Arc<watch::Sender<u64>>, ReleaseError> {
    type Registry = HashMap<PathBuf, Weak<watch::Sender<u64>>>;
    static REGISTRY: OnceLock<Mutex<Registry>> = OnceLock::new();
    let key = std::fs::canonicalize(directory)
        .map_err(|_| ReleaseError::from(StateError::new("state_dir_invalid")))?;
    let mut registry = REGISTRY
        .get_or_init(Mutex::default)
        .lock()
        .map_err(|_| corrupt())?;
    registry.retain(|_, sender| sender.strong_count() != 0);
    if let Some(sender) = registry.get(&key).and_then(Weak::upgrade) {
        return Ok(sender);
    }
    let (sender, _) = watch::channel(0);
    let sender = Arc::new(sender);
    registry.insert(key, Arc::downgrade(&sender));
    Ok(sender)
}
