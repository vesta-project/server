//! Audited pack-access passwords, never identity or management credentials.
//!
//! Current canonical authority and the expected revision are checked before KDF admission
//! and again in the admitted mutation transaction. Derivation never holds a database
//! transaction. A verification match is only returned after rechecking the stored verifier
//! and committing its audit; it creates no principal, device, role, session or grant.
use super::password::{
    GateConfig, GateError, HashPolicy, KdfGate, PasswordError, PasswordHasher, Purpose,
    SecretPassword,
};
use super::repository::Repository;
use super::{Capability, Decision, EpochMillis, Subject};
use crate::{ControlError, IdentityErrorCode, Result};
use rusqlite::{Connection, params, types::ValueRef};
use std::sync::{Arc, OnceLock};
use vesta_state::audit::{AuditActor, AuditLog, AuditRecord};
use vesta_state::{StateDatabase, StateError};
use zeroize::Zeroizing;

#[cfg(test)]
#[path = "tests/pack_passwords.rs"]
mod tests;

const SOURCE: &str = "pack-password";
const TARGET: &str = "pack-password";
const SET: &str = "pack_password.set";
const REVOKE: &str = "pack_password.revoke";
const INSPECT: &str = "pack_password.inspect";
const VERIFY: &str = "pack_password.verify";
const MAX_VERIFIER_BYTES: usize = 256;

/// Zero denotes absence only. Persisted rows always have a positive revision; tombstones
/// retain it, so revocation cannot reset the credential to a reusable creation revision.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PasswordRevision(i64);
impl PasswordRevision {
    pub const ABSENT: Self = Self(0);
    pub fn new(value: u64) -> Result<Self> {
        i64::try_from(value)
            .map(Self)
            .map_err(|_| ControlError::rejected(IdentityErrorCode::InvalidRequest))
    }
    pub const fn value(self) -> u64 {
        self.0 as u64
    }
    fn next(self) -> Result<Self> {
        self.0
            .checked_add(1)
            .map(Self)
            .ok_or_else(|| ControlError::rejected(IdentityErrorCode::CredentialRevisionExhausted))
    }
}

/// Administrative observation without any verifier material or inbound deserializer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PasswordMetadata {
    server_id: String,
    revision: PasswordRevision,
    active: bool,
    created_at: Option<EpochMillis>,
    updated_at: Option<EpochMillis>,
    revoked_at: Option<EpochMillis>,
}
impl PasswordMetadata {
    pub fn server_id(&self) -> &str {
        &self.server_id
    }
    pub const fn revision(&self) -> PasswordRevision {
        self.revision
    }
    pub const fn active(&self) -> bool {
        self.active
    }
    pub const fn created_at(&self) -> Option<EpochMillis> {
        self.created_at
    }
    pub const fn updated_at(&self) -> Option<EpochMillis> {
        self.updated_at
    }
    pub const fn revoked_at(&self) -> Option<EpochMillis> {
        self.revoked_at
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PasswordVerificationStatus {
    Match,
    Mismatch,
    Unavailable,
    Changed,
    InvalidPassword,
    InvalidVerifier,
    KdfUnavailable,
    RateLimited,
    Busy,
}
impl PasswordVerificationStatus {
    const fn wire(self) -> &'static str {
        match self {
            Self::Match => "match",
            Self::Mismatch => "mismatch",
            Self::Unavailable => "unavailable",
            Self::Changed => "changed",
            Self::InvalidPassword => "invalid_password",
            Self::InvalidVerifier => "invalid_verifier",
            Self::KdfUnavailable => "kdf_unavailable",
            Self::RateLimited => "rate_limited",
            Self::Busy => "busy",
        }
    }
}

/// Not a session, authorization token or proof accepted by any management command.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PasswordVerification {
    server_id: String,
    revision: PasswordRevision,
    status: PasswordVerificationStatus,
}
impl PasswordVerification {
    #[cfg(test)]
    pub(crate) fn for_test(
        server_id: &str,
        revision: PasswordRevision,
        status: PasswordVerificationStatus,
    ) -> Self {
        Self {
            server_id: server_id.to_owned(),
            revision,
            status,
        }
    }
    pub fn server_id(&self) -> &str {
        &self.server_id
    }
    pub const fn revision(&self) -> PasswordRevision {
        self.revision
    }
    pub const fn status(&self) -> PasswordVerificationStatus {
        self.status
    }
}

#[derive(Clone)]
pub struct PackPasswords {
    db: StateDatabase,
    audit: AuditLog,
    hasher: Arc<PasswordHasher>,
    gate: KdfGate,
    #[cfg(test)]
    before_commit: Option<Arc<dyn Fn() + Send + Sync>>,
}
impl PackPasswords {
    /// All production instances share the process budget, including different databases.
    /// A new service is not a rate reset. Transport/source abuse controls remain separate.
    pub fn new(db: StateDatabase) -> Result<Self> {
        static GATE: OnceLock<KdfGate> = OnceLock::new();
        let gate = GATE
            .get_or_init(|| {
                KdfGate::new(GateConfig::default())
                    .expect("fixed valid password gate configuration")
            })
            .clone();
        Self::compose(db, HashPolicy::default(), gate)
    }

    fn compose(db: StateDatabase, policy: HashPolicy, gate: KdfGate) -> Result<Self> {
        let audit = AuditLog::new(db.clone())?;
        let hasher = PasswordHasher::new(policy).map_err(kdf_error)?;
        Ok(Self {
            db,
            audit,
            hasher: Arc::new(hasher),
            gate,
            #[cfg(test)]
            before_commit: None,
        })
    }

    /// Trusted pack-access policy input: the active revision, or `None` when absent or
    /// revoked. Exposes no verifier and is not an administrative observation.
    pub(crate) fn active_revision(&self) -> Result<Option<PasswordRevision>> {
        let row = self
            .db
            .read_with(|transaction| load(transaction.connection(), self.db.server_id()))
            .map_err(ControlError::from)?;
        Ok(row
            .filter(|row| row.revoked_at.is_none())
            .map(|row| row.revision))
    }

    pub fn metadata(&self, subject: &Subject) -> Result<PasswordMetadata> {
        self.administrative(subject, INSPECT, || {
            self.db
                .read_with(|transaction| {
                    self.require_authority(transaction.connection(), subject)?;
                    Ok(
                        self.describe(
                            load(transaction.connection(), self.db.server_id())?.as_ref(),
                        ),
                    )
                })
                .map_err(ControlError::from)
        })
    }

    /// Create at ABSENT, rotate/reactivate at the exact positive revision. No automatic
    /// rehash or downgrade occurs, and the caller retains ownership of their secret guard.
    pub fn set(
        &self,
        subject: &Subject,
        expected: PasswordRevision,
        password: &SecretPassword,
    ) -> Result<PasswordMetadata> {
        self.administrative(subject, SET, || {
            self.precheck(subject, expected, true)?;
            let _permit = self.gate.try_acquire(Purpose::Management).map_err(gate_error)?;
            let verifier = self.hasher.hash(password).map_err(kdf_error)?;
            self.test_before_commit();
            self.db.write_with(|transaction| {
                let connection = transaction.connection();
                let now = self.require_authority(connection, subject)?;
                let current = load(connection, self.db.server_id())?;
                require_revision(current.as_ref(), expected, true)?;
                let revision = expected.next()?;
                if current.is_none() {
                    connection.execute(
                        "INSERT INTO pack_passwords (server_id,revision,phc,created_at,updated_at) VALUES (?,?,?,?,?)",
                        params![self.db.server_id(), revision.0, verifier.as_str(), now, now],
                    )?;
                } else {
                    let changed = connection.execute(
                        "UPDATE pack_passwords SET revision=?,phc=?,updated_at=?,revoked_at=NULL WHERE server_id=? AND revision=?",
                        params![revision.0, verifier.as_str(), now, self.db.server_id(), expected.0],
                    )?;
                    if changed != 1 { return Err(conflict()); }
                }
                self.mutation_audit(connection, subject, SET, revision, expected)?;
                Ok(self.describe(load(connection, self.db.server_id())?.as_ref()))
            }).map_err(ControlError::from)
        })
    }

    /// Nulls the current verifier, retaining creation time and a monotonically increasing
    /// tombstone. Historical SQLite/WAL/backups are not securely erased by this statement.
    pub fn revoke(
        &self,
        subject: &Subject,
        expected: PasswordRevision,
    ) -> Result<PasswordMetadata> {
        self.administrative(subject, REVOKE, || {
            self.precheck(subject, expected, false)?;
            self.gate.charge_management().map_err(gate_error)?;
            self.test_before_commit();
            self.db.write_with(|transaction| {
                let connection = transaction.connection();
                let now = self.require_authority(connection, subject)?;
                let current = load(connection, self.db.server_id())?;
                require_revision(current.as_ref(), expected, false)?;
                let revision = expected.next()?;
                let changed = connection.execute(
                    "UPDATE pack_passwords SET revision=?,phc=NULL,updated_at=?,revoked_at=? WHERE server_id=? AND revision=?",
                    params![revision.0, now, now, self.db.server_id(), expected.0],
                )?;
                if changed != 1 { return Err(conflict()); }
                self.mutation_audit(connection, subject, REVOKE, revision, expected)?;
                Ok(self.describe(load(connection, self.db.server_id())?.as_ref()))
            }).map_err(ControlError::from)
        })
    }

    /// Consumes owned bytes so even invalid inputs are guarded/wiped. Rate is charged
    /// before password validation or storage inspection; busy/rate refusals produce no
    /// audit flood. Invalid, missing, revoked and malformed admitted attempts are audited.
    pub fn verify(&self, password: Vec<u8>) -> Result<PasswordVerification> {
        let mut password = Zeroizing::new(password);
        let _permit = match self.gate.try_acquire(Purpose::Verification) {
            Ok(permit) => permit,
            Err(GateError::RateLimited) => {
                return Ok(self.verification(
                    PasswordRevision::ABSENT,
                    PasswordVerificationStatus::RateLimited,
                ));
            }
            Err(GateError::Busy) => {
                return Ok(
                    self.verification(PasswordRevision::ABSENT, PasswordVerificationStatus::Busy)
                );
            }
            Err(error) => return Err(gate_error(error)),
        };
        let snapshot = self
            .db
            .read_with(|transaction| load(transaction.connection(), self.db.server_id()))
            .map_err(ControlError::from)?;
        // Move, not clone, the original allocation into the validated secret guard.
        let secret = SecretPassword::try_from(std::mem::take(&mut *password));
        let computed = match secret {
            Err(_) => PasswordVerificationStatus::InvalidPassword,
            Ok(secret) => match snapshot.as_ref().and_then(|row| row.phc.as_ref()) {
                None => PasswordVerificationStatus::Unavailable,
                Some(phc) => match self.hasher.verify(&secret, phc) {
                    Ok(true) => PasswordVerificationStatus::Match,
                    Ok(false) => PasswordVerificationStatus::Mismatch,
                    Err(
                        PasswordError::MalformedVerifier
                        | PasswordError::UnsupportedVerifier
                        | PasswordError::CostOutOfBounds,
                    ) => PasswordVerificationStatus::InvalidVerifier,
                    Err(PasswordError::InvalidPassword) => {
                        PasswordVerificationStatus::InvalidPassword
                    }
                    Err(_) => PasswordVerificationStatus::KdfUnavailable,
                },
            },
        };
        self.test_before_commit();
        self.db
            .write_with(|transaction| {
                let connection = transaction.connection();
                let current = load(connection, self.db.server_id())?;
                let status = if snapshot == current {
                    computed
                } else {
                    PasswordVerificationStatus::Changed
                };
                let revision = snapshot
                    .as_ref()
                    .map_or(PasswordRevision::ABSENT, |row| row.revision);
                let record = AuditRecord::new(
                    AuditActor::system(SOURCE.to_owned())?,
                    VERIFY.to_owned(),
                    TARGET.to_owned(),
                    status.wire().to_owned(),
                    None,
                    [("revision".to_owned(), Some(revision.0.to_string()))],
                )?;
                self.audit.append(connection, &record)?;
                Ok(self.verification(revision, status))
            })
            .map_err(ControlError::from)
    }

    fn precheck(
        &self,
        subject: &Subject,
        expected: PasswordRevision,
        creation: bool,
    ) -> Result<()> {
        self.db
            .read_with(|transaction| {
                self.require_authority(transaction.connection(), subject)?;
                require_revision(
                    load(transaction.connection(), self.db.server_id())?.as_ref(),
                    expected,
                    creation,
                )?;
                expected.next()?;
                Ok(())
            })
            .map_err(ControlError::from)
    }

    fn require_authority(&self, connection: &Connection, subject: &Subject) -> Result<EpochMillis> {
        // Called only within established snapshots/admitted writers, never before admission.
        let now = self.db.clock().millis();
        let policy = Repository::new(connection, self.db.server_id(), now).policy(Some(subject))?;
        match policy.check(Capability::CredentialsManage, None) {
            Decision::Allow => Ok(now),
            Decision::Deny(reason) => Err(ControlError::forbidden(reason)),
        }
    }

    fn mutation_audit(
        &self,
        connection: &Connection,
        subject: &Subject,
        action: &str,
        revision: PasswordRevision,
        previous: PasswordRevision,
    ) -> Result<()> {
        self.audit.append(
            connection,
            &AuditRecord::new(
                claimed_actor(subject)?,
                action.to_owned(),
                TARGET.to_owned(),
                "success".to_owned(),
                None,
                [
                    ("revision".to_owned(), Some(revision.0.to_string())),
                    ("previous_revision".to_owned(), Some(previous.0.to_string())),
                ],
            )?,
        )?;
        Ok(())
    }

    fn administrative<T>(
        &self,
        subject: &Subject,
        action: &str,
        work: impl FnOnce() -> Result<T>,
    ) -> Result<T> {
        match work() {
            Err(primary @ ControlError::Rejected { code, reason })
                if matches!(
                    code,
                    IdentityErrorCode::Forbidden
                        | IdentityErrorCode::CredentialConflict
                        | IdentityErrorCode::CredentialRevisionExhausted
                        | IdentityErrorCode::InvalidRequest
                ) =>
            {
                // A plain domain error means clean rollback/snapshot disposal. Infrastructure
                // and rollback errors remain infrastructure; local admission failures are not audited.
                let audit = claimed_actor(subject)
                    .and_then(|actor| {
                        AuditRecord::new(
                            actor,
                            action.to_owned(),
                            TARGET.to_owned(),
                            "denied".to_owned(),
                            None,
                            [(
                                "reason".to_owned(),
                                Some(
                                    reason
                                        .map_or(code.code(), |reason| reason.code())
                                        .to_owned(),
                                ),
                            )],
                        )
                    })
                    .and_then(|record| self.audit.append_record(&record));
                Err(match audit {
                    Ok(_) => primary,
                    Err(error) => ControlError::AuditUnavailable {
                        primary: Box::new(primary),
                        audit_failure: Box::new(ControlError::State(error)),
                    },
                })
            }
            other => other,
        }
    }

    fn describe(&self, row: Option<&Snapshot>) -> PasswordMetadata {
        PasswordMetadata {
            server_id: self.db.server_id().to_owned(),
            revision: row.map_or(PasswordRevision::ABSENT, |row| row.revision),
            active: row.is_some_and(|row| row.revoked_at.is_none()),
            created_at: row.map(|row| row.created_at),
            updated_at: row.map(|row| row.updated_at),
            revoked_at: row.and_then(|row| row.revoked_at),
        }
    }
    fn verification(
        &self,
        revision: PasswordRevision,
        status: PasswordVerificationStatus,
    ) -> PasswordVerification {
        PasswordVerification {
            server_id: self.db.server_id().to_owned(),
            revision,
            status,
        }
    }
    fn test_before_commit(&self) {
        #[cfg(test)]
        if let Some(hook) = &self.before_commit {
            hook();
        }
    }
}

// Never formatted or exposed. Owned verifier copies are wiped on normal/error/unwind exits.
#[derive(PartialEq, Eq)]
struct Snapshot {
    revision: PasswordRevision,
    phc: Option<Zeroizing<String>>,
    created_at: EpochMillis,
    updated_at: EpochMillis,
    revoked_at: Option<EpochMillis>,
}
fn load(connection: &Connection, server: &str) -> Result<Option<Snapshot>> {
    let mut statement = connection.prepare(
        "SELECT revision,phc,created_at,updated_at,revoked_at FROM pack_passwords WHERE server_id=?",
    )?;
    let mut rows = statement.query([server])?;
    let Some(row) = rows.next()? else {
        return Ok(None);
    };
    let revision: i64 = row.get(0)?;
    if revision < 1 {
        return Err(corrupt());
    }
    // length(TEXT) stops at NUL. Inspect the complete borrowed SQLite bytes BEFORE copying.
    let phc = match row.get_ref(1)? {
        ValueRef::Null => None,
        ValueRef::Text(bytes) if !bytes.is_empty() && bytes.len() <= MAX_VERIFIER_BYTES => {
            let text = std::str::from_utf8(bytes).map_err(|_| corrupt())?;
            Some(Zeroizing::new(text.to_owned()))
        }
        _ => return Err(corrupt()),
    };
    let revoked_at: Option<i64> = row.get(4)?;
    if revoked_at.is_none() != phc.is_some() {
        return Err(corrupt());
    }
    Ok(Some(Snapshot {
        revision: PasswordRevision(revision),
        phc,
        created_at: row.get(2)?,
        updated_at: row.get(3)?,
        revoked_at,
    }))
}
fn require_revision(
    row: Option<&Snapshot>,
    expected: PasswordRevision,
    creation: bool,
) -> Result<()> {
    if !creation && expected == PasswordRevision::ABSENT {
        return Err(ControlError::rejected(IdentityErrorCode::InvalidRequest));
    }
    if row.map_or(PasswordRevision::ABSENT, |row| row.revision) != expected {
        return Err(conflict());
    }
    Ok(())
}
fn claimed_actor(subject: &Subject) -> vesta_state::Result<AuditActor> {
    AuditActor::new(
        subject.principal.as_str().to_owned(),
        subject.device.as_str().to_owned(),
        SOURCE.to_owned(),
    )
}
fn conflict() -> ControlError {
    ControlError::rejected(IdentityErrorCode::CredentialConflict)
}
fn corrupt() -> ControlError {
    StateError::new("state_credential_invalid").into()
}
fn kdf_error(error: PasswordError) -> ControlError {
    ControlError::rejected(match error {
        PasswordError::InvalidPassword => IdentityErrorCode::InvalidPassword,
        _ => IdentityErrorCode::PasswordKdfUnavailable,
    })
}
fn gate_error(error: GateError) -> ControlError {
    match error {
        GateError::RateLimited => ControlError::rejected(IdentityErrorCode::PasswordRateLimited),
        GateError::Busy => ControlError::rejected(IdentityErrorCode::PasswordKdfBusy),
        GateError::Closed => StateError::new("state_password_gate_closed").into(),
        _ => ControlError::rejected(IdentityErrorCode::PasswordKdfUnavailable),
    }
}
