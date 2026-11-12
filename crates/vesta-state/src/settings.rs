//! Plain desired/effective settings. Desired changes require explicit adapter acknowledgement.
//! This is trusted persistence, not a remote configuration or authorization boundary.
use crate::{
    Result, StateDatabase, StateError,
    audit::{AuditActor, AuditLog, AuditRecord},
    values,
};
use rusqlite::{Connection, Row, params};

pub const MAX_VALUE_CHARS: usize = 4_096;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ApplyMode {
    Immediate,
    OnRestart,
    Manual,
}

impl ApplyMode {
    pub const fn wire(self) -> &'static str {
        match self {
            Self::Immediate => "immediate",
            Self::OnRestart => "on_restart",
            Self::Manual => "manual",
        }
    }

    pub fn from_wire(value: &str) -> Result<Self> {
        if value.eq_ignore_ascii_case("immediate") {
            Ok(Self::Immediate)
        } else if value.eq_ignore_ascii_case("on_restart") {
            Ok(Self::OnRestart)
        } else if value.eq_ignore_ascii_case("manual") {
            Ok(Self::Manual)
        } else {
            Err(StateError::new("state_invalid_request"))
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Setting {
    pub key: String,
    pub desired_value: String,
    pub desired_revision: i64,
    pub apply_mode: ApplyMode,
    pub effective_value: Option<String>,
    pub effective_revision: i64,
    pub updated_at_ms: i64,
}

impl Setting {
    pub fn pending(&self) -> bool {
        self.effective_revision != self.desired_revision
    }
}

#[derive(Clone)]
pub struct SettingsStore {
    db: StateDatabase,
    audit: AuditLog,
}

impl SettingsStore {
    /// Derives audit identity and clock from this database; a foreign audit log cannot be supplied.
    pub fn new(db: StateDatabase) -> Result<Self> {
        let audit = AuditLog::new(db.clone())?;
        Ok(Self { db, audit })
    }

    pub fn set_desired(
        &self,
        actor: &AuditActor,
        key: &str,
        value: &str,
        mode: ApplyMode,
    ) -> Result<Setting> {
        values::require_text(key, 128)?;
        if values::utf16_len(value) > MAX_VALUE_CHARS {
            return Err(StateError::new("state_invalid_request"));
        }
        self.db.write(|c| {
            c.execute(
                "INSERT INTO settings (server_id,key,desired_value,desired_revision,apply_mode,updated_at) VALUES (?,?,?,1,?,?) \
                 ON CONFLICT (server_id,key) DO UPDATE SET desired_value=excluded.desired_value,\
                 desired_revision=settings.desired_revision+1,apply_mode=excluded.apply_mode,updated_at=excluded.updated_at",
                params![self.db.server_id(), key, value, mode.wire(), self.db.clock().millis()],
            )?;
            let setting = self.load(c, key)?.ok_or_else(|| StateError::new("state_not_found"))?;
            self.audit.append(c, &AuditRecord::new(
                actor.clone(), "settings.set_desired".into(), format!("setting:{key}"), "ok".into(), None,
                [("revision".into(), Some(setting.desired_revision.to_string())),
                 ("applyMode".into(), Some(mode.wire().into()))],
            )?)?;
            Ok(setting)
        })
    }

    /// Acknowledges only the current desired revision, including for Immediate settings.
    pub fn mark_effective(&self, actor: &AuditActor, key: &str, revision: i64) -> Result<Setting> {
        self.db.write(|c| {
            let setting = self.load(c, key)?.ok_or_else(|| StateError::new("state_not_found"))?;
            if revision != setting.desired_revision { return Err(StateError::new("state_stale_revision")); }
            let changed = c.execute(
                "UPDATE settings SET effective_value=desired_value,effective_revision=desired_revision,updated_at=? \
                 WHERE server_id=? AND key=? AND desired_revision=?",
                params![self.db.clock().millis(), self.db.server_id(), key, revision],
            )?;
            if changed != 1 { return Err(StateError::new("state_stale_revision")); }
            self.audit.append(c, &AuditRecord::new(
                actor.clone(), "settings.mark_effective".into(), format!("setting:{key}"), "ok".into(), None,
                [("revision".into(), Some(revision.to_string()))],
            )?)?;
            self.load(c, key)?.ok_or_else(|| StateError::new("state_not_found"))
        })
    }

    pub fn get(&self, key: &str) -> Result<Option<Setting>> {
        self.db.read(|c| self.load(c, key))
    }

    pub fn pending(&self) -> Result<Vec<Setting>> {
        self.db.read(|c| {
            let mut statement = c.prepare(
                "SELECT key,desired_value,desired_revision,apply_mode,effective_value,effective_revision,updated_at \
                 FROM settings WHERE server_id=? AND desired_revision<>effective_revision ORDER BY key",
            )?;
            let mut rows = statement.query([self.db.server_id()])?;
            let mut out = Vec::new();
            while let Some(row) = rows.next()? { out.push(map_row(row)?); }
            Ok(out)
        })
    }

    fn load(&self, c: &Connection, key: &str) -> Result<Option<Setting>> {
        let mut statement = c.prepare(
            "SELECT key,desired_value,desired_revision,apply_mode,effective_value,effective_revision,updated_at \
             FROM settings WHERE server_id=? AND key=?",
        )?;
        let mut rows = statement.query(params![self.db.server_id(), key])?;
        rows.next()?.map(map_row).transpose()
    }
}

fn map_row(row: &Row<'_>) -> Result<Setting> {
    let wire: String = row.get(3)?;
    Ok(Setting {
        key: row.get(0)?,
        desired_value: row.get(1)?,
        desired_revision: row.get(2)?,
        apply_mode: ApplyMode::from_wire(&wire)?,
        effective_value: row.get(4)?,
        effective_revision: row.get(5)?,
        updated_at_ms: row.get(6)?,
    })
}
