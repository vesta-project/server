//! Remote recovery export (F27): the owner downloads the credential-free control data
//! through the management API, optionally encrypted with a passphrase they choose.
//!
//! The export is built in memory and bounded; a larger one must be taken locally with
//! `vesta-agent recovery-export`, which streams to a file. The request is authorized
//! (owner-reserved `recovery.export`) and audited before any data is produced. The
//! passphrase is never stored or audited.
use crate::identity::{self, Capability, Subject};
use std::io::{self, Write};
use vesta_state::{
    StateDatabase,
    audit::{AuditActor, AuditLog, AuditRecord},
};

/// Exports larger than this must use the local command.
pub const MAX_REMOTE_EXPORT_BYTES: usize = 64 * 1024 * 1024;
pub const MIN_PASSPHRASE_BYTES: usize = 8;
pub const MAX_PASSPHRASE_BYTES: usize = 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecoveryExportError {
    Forbidden,
    /// The passphrase is too short or too long.
    Invalid,
    TooLarge,
    Unavailable,
}
impl RecoveryExportError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::Forbidden => "recovery_export_forbidden",
            Self::Invalid => "recovery_export_passphrase_invalid",
            Self::TooLarge => "recovery_export_too_large_use_local_command",
            Self::Unavailable => "recovery_export_unavailable",
        }
    }
}

/// A `Vec` that refuses to grow past a bound and remembers that it did.
#[derive(Default)]
struct Bounded {
    bytes: Vec<u8>,
    overflowed: bool,
}
impl Write for Bounded {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.bytes.len() + bytes.len() > MAX_REMOTE_EXPORT_BYTES {
            self.overflowed = true;
            return Err(io::Error::new(
                io::ErrorKind::FileTooLarge,
                "export too large",
            ));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Authorize and audit, then produce the export (plain JSON, or the encrypted format
/// when a passphrase is given).
pub fn export_for(
    db: &StateDatabase,
    subject: &Subject,
    passphrase: Option<&[u8]>,
) -> Result<Vec<u8>, RecoveryExportError> {
    if passphrase.is_some_and(|p| !(MIN_PASSPHRASE_BYTES..=MAX_PASSPHRASE_BYTES).contains(&p.len()))
    {
        return Err(RecoveryExportError::Invalid);
    }
    let audit = AuditLog::new(db.clone()).map_err(|_| RecoveryExportError::Unavailable)?;
    let encrypted = passphrase.is_some();
    db.write(|c| {
        let policy = identity::policy_in(c, db.server_id(), db.clock().millis(), Some(subject))
            .map_err(|_| vesta_state::StateError::new("state_policy_unavailable"))?;
        if !policy.check(Capability::RecoveryExport, None).allowed() {
            return Ok(Err(RecoveryExportError::Forbidden));
        }
        let record = AuditRecord::new(
            AuditActor::new(
                subject.principal.as_str().into(),
                subject.device.as_str().into(),
                "recovery".into(),
            )?,
            "recovery.export".into(),
            "recovery-export".into(),
            "accepted".into(),
            None,
            [("encrypted".into(), Some(encrypted.to_string()))],
        )?;
        audit.append(c, &record)?;
        Ok(Ok(()))
    })
    .map_err(|_| RecoveryExportError::Unavailable)??;
    let mut out = Bounded::default();
    match vesta_state::export::write_recovery_export(db, &mut out, passphrase) {
        Ok(_) => Ok(out.bytes),
        Err(_) if out.overflowed => Err(RecoveryExportError::TooLarge),
        Err(_) => Err(RecoveryExportError::Unavailable),
    }
}
