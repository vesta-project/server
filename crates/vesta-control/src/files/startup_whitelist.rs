//! Durable desired membership while a particular game generation starts.
//! No Minecraft file writes, new thread, or work when the queue is empty.
use super::*;
use crate::{
    moderation::{self, ModerationAction, WhitelistEntry},
    operations::{
        Admission, GameCommand, GenerationId, OperationId, OperationState, RuntimeHandle,
        readiness::Readiness,
    },
};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::{
    Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use vesta_state::audit::{AuditActor, AuditRecord};

const KEY: &str = "whitelist-runtime";
const LIMIT: usize = 32;
const VERIFY_MS: i64 = 20_000;
pub(super) struct StartupState {
    lock: Mutex<()>,
    pending: AtomicBool,
    cursor: AtomicUsize,
}
impl StartupState {
    pub(super) fn open(db: &StateDatabase) -> Result<Self> {
        let record = db
            .read_with(|s| load(s.connection(), db.server_id()))
            .map_err(FileError::from)?;
        Ok(Self {
            lock: Mutex::new(()),
            pending: AtomicBool::new(!record.entries.is_empty()),
            cursor: AtomicUsize::new(0),
        })
    }
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct StartupWhitelistPatch {
    pub player: String,
    pub add: bool,
    pub expected_generation: String,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Change {
    id: String,
    key: String,
    player: String,
    uuid: String,
    add: bool,
    generation: String,
    principal: String,
    device: String,
    online: bool,
    dispatching: bool,
    operation: Option<String>,
    submitted_at: Option<i64>,
    verification_since: Option<i64>,
}
impl Change {
    fn subject(&self) -> Result<Subject> {
        Ok(Subject {
            principal: identity::PrincipalId::parse(&self.principal)?,
            device: identity::DeviceId::parse(&self.device)?,
        })
    }
}
#[derive(Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Record {
    entries: Vec<Change>,
}
fn load(c: &Connection, server: &str) -> Result<Record> {
    let text: Option<String> = c
        .query_row(
            "SELECT desired_value FROM settings WHERE server_id=? AND key=?",
            params![server, KEY],
            |r| r.get(0),
        )
        .optional()?;
    let Some(text) = text else {
        return Ok(Record::default());
    };
    if text.len() > 64 * 1024 {
        return Err(FileError::Corrupt);
    }
    let record: Record = serde_json::from_str(&text).map_err(|_| FileError::Corrupt)?;
    if record.entries.len() > LIMIT {
        return Err(FileError::Corrupt);
    }
    for e in &record.entries {
        if !moderation::valid_player(&e.player)
            || uuid::Uuid::parse_str(&e.uuid).is_err()
            || uuid::Uuid::parse_str(&e.id).is_err()
            || GenerationId::parse_hex(&e.generation).is_err()
            || e.key.is_empty()
            || e.key.len() > 128
            || !e.key.bytes().all(|b| b.is_ascii_graphic())
            || (e.operation.is_some() && !e.dispatching)
        {
            return Err(FileError::Corrupt);
        }
        e.subject()?;
        if let Some(id) = &e.operation {
            OperationId::parse(id).map_err(|_| FileError::Corrupt)?;
        }
    }
    Ok(record)
}
fn save(c: &Connection, db: &StateDatabase, record: &Record) -> Result<()> {
    let text = serde_json::to_string(record).map_err(|_| FileError::Corrupt)?;
    if record.entries.len() > LIMIT || text.len() > 64 * 1024 {
        return Err(FileError::IntentsFull);
    }
    c.execute("INSERT INTO settings(server_id,key,desired_value,desired_revision,apply_mode,effective_value,effective_revision,updated_at) VALUES(?,?,?,1,'immediate',?,1,?) ON CONFLICT(server_id,key) DO UPDATE SET desired_value=excluded.desired_value,desired_revision=settings.desired_revision+1,effective_value=excluded.desired_value,effective_revision=settings.desired_revision+1,updated_at=excluded.updated_at",params![db.server_id(),KEY,text,text,db.clock().millis()])?;
    Ok(())
}
fn online(files: &FileBrowser) -> Result<bool> {
    crate::minecraft_admission::ServerProperties::load(
        &files.root.raw_path().join("server.properties"),
    )
    .map(|p| p.online_mode())
    .map_err(|_| FileError::Unavailable)
}
fn audit(
    files: &FileBrowser,
    c: &Connection,
    subject: &Subject,
    action: &str,
    id: &str,
) -> Result<()> {
    files.audit.append(
        c,
        &AuditRecord::new(
            AuditActor::new(
                subject.principal.as_str().into(),
                subject.device.as_str().into(),
                "players".into(),
            )?,
            action.into(),
            "whitelist".into(),
            "accepted".into(),
            None,
            [("intent".into(), Some(id.into()))],
        )?,
    )?;
    Ok(())
}
fn same_player(e: &Change, player: &str, generation: &str) -> bool {
    e.generation == generation && e.player.eq_ignore_ascii_case(player)
}
fn receipt(c: &Connection, db: &StateDatabase, e: &Change, status: &str) -> Result<()> {
    let changed=c.execute("UPDATE management_file_outcomes SET outcome_json=json_set(outcome_json,'$.status',?),completed_at=? WHERE server_id=? AND principal_id=? AND device_id=? AND request_key=? AND json_extract(outcome_json,'$.intent_id')=?",params![status,db.clock().millis(),db.server_id(),e.principal,e.device,e.key,e.id])?;
    if changed != 1 {
        return Err(FileError::Unavailable);
    }
    Ok(())
}
fn coalesce(record: &mut Record, change: Change, base: bool) -> Result<()> {
    // Cancel only effects that have not crossed the durable dispatch boundary.
    record
        .entries
        .retain(|e| !same_player(e, &change.player, &change.generation) || e.dispatching);
    let in_flight = record
        .entries
        .iter()
        .any(|e| same_player(e, &change.player, &change.generation));
    if base != change.add || in_flight {
        if record.entries.len() >= LIMIT {
            return Err(FileError::IntentsFull);
        }
        record.entries.push(change);
    }
    Ok(())
}
#[cfg(test)]
thread_local! {
    static BEFORE_LOCK: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
}
impl FileBrowser {
    #[cfg(test)]
    pub(crate) fn before_startup_whitelist_lock_for_test(hook: impl FnOnce() + 'static) {
        BEFORE_LOCK.with(|slot| *slot.borrow_mut() = Some(Box::new(hook)));
    }
    pub fn startup_whitelist_update(
        &self,
        runtime: &RuntimeHandle,
        subject: &Subject,
        key: &str,
        patch: StartupWhitelistPatch,
    ) -> Result<Value> {
        if !moderation::valid_player(&patch.player)
            || GenerationId::parse_hex(&patch.expected_generation).is_err()
        {
            return Err(FileError::InvalidPath);
        }
        self.check_global_authority(subject, Capability::PlayersModerate)?;
        let files = self.remote_preparation();
        let input = json!(["startup-whitelist", patch]);
        if let Some(outcome) = files.remote_replay_before_preparation(subject, key, &input)? {
            return Ok(outcome);
        }
        let mode = online(&files)?;
        // External profile IO must not occupy the global mutation admission lane.
        let resolved = if patch.add {
            Some(super::offline_whitelist::resolve_bounded(
                &patch.player,
                mode,
                files.remote_deadline,
            )?)
        } else {
            None
        };
        files.execute_remote(subject,key,input,||Ok(vec![(Capability::PlayersModerate.id().into(),String::new())]),|files| {
            let snapshot=runtime.game_snapshot().map_err(|_|FileError::Unavailable)?;
            if snapshot.phase!=crate::operations::ProcessPhase::Running || snapshot.generation.map(|g|g.to_hex()).as_deref()!=Some(&patch.expected_generation) || matches!(runtime.readiness().map_err(|_|FileError::Unavailable)?.readiness,Readiness::Degraded(_)) {return Err(FileError::Conflict(None))}
            if online(files)?!=mode {return Err(FileError::Conflict(None))}
            // Disk membership and prior desires must be observed together,
            // after any completing coordinator tick.
            #[cfg(test)]
            BEFORE_LOCK.with(|hook| { if let Some(hook)=hook.borrow_mut().take() {hook();} });
            let _guard=files.startup_whitelist.lock.lock().map_err(|_|FileError::Unavailable)?;
            let actual=moderation::whitelist(files.root.raw_path()).map_err(|_|FileError::Corrupt)?;
            let mut names=std::collections::HashSet::new();let mut uuids=std::collections::HashSet::new();
            if actual.iter().any(|e|!names.insert(e.name.to_ascii_lowercase()) || !uuids.insert(uuid::Uuid::parse_str(&e.uuid).ok())) {return Err(FileError::Corrupt)}
            let existing=actual.iter().find(|e|e.name.eq_ignore_ascii_case(&patch.player));
            let (name,uuid)=if patch.add {
                let resolved=resolved.ok_or(FileError::Unavailable)?;
                if existing.is_some_and(|e|uuid::Uuid::parse_str(&e.uuid).ok()!=uuid::Uuid::parse_str(&resolved.1).ok()) {return Err(FileError::Conflict(None))}
                resolved
            } else {
                // Removal after an unsubmitted addition can use its resolved identity.
                let record=files.db.read_with(|s|load(s.connection(),files.db.server_id())).map_err(FileError::from)?;
                let prior=record.entries.iter().rev().find(|e|same_player(e,&patch.player,&patch.expected_generation));
                match (prior,existing) { (Some(e),_) =>(e.player.clone(),e.uuid.clone()),(_,Some(e))=>(e.name.clone(),e.uuid.clone()),_ =>(patch.player.clone(),uuid::Uuid::nil().to_string()) }
            };
            let id=uuid::Uuid::new_v4().to_string();
            let result=files.db.write_with(|tx| {
                files.check_remote_completion_authority(tx.connection(),subject)?;
                let snapshot=runtime.game_snapshot().map_err(|_|FileError::Unavailable)?;
                if snapshot.phase!=crate::operations::ProcessPhase::Running || snapshot.generation.map(|g|g.to_hex()).as_deref()!=Some(&patch.expected_generation) || online(files)?!=mode {return Err(FileError::Conflict(None))}
                let mut record=load(tx.connection(),files.db.server_id())?;
                let previous=record.entries.clone();
                let base=actual.iter().any(|e|uuid::Uuid::parse_str(&e.uuid).ok()==uuid::Uuid::parse_str(&uuid).ok());
                coalesce(&mut record,Change {id:id.clone(),key:key.into(),player:name.clone(),uuid:uuid.clone(),add:patch.add,generation:patch.expected_generation.clone(),principal:subject.principal.as_str().into(),device:subject.device.as_str().into(),online:mode,dispatching:false,operation:None,submitted_at:None,verification_since:None},base)?;
                for prior in previous.iter().filter(|e|!record.entries.iter().any(|new|new.id==e.id)) {receipt(tx.connection(),&files.db,prior,"cancelled")?;}
                save(tx.connection(),&files.db,&record)?;
                audit(files,tx.connection(),subject,"players.whitelist.desired",&id)?;
                let status=if record.entries.iter().any(|e|e.id==id) {"queued"} else {"cancelled"};
                let outcome=json!({"outcome":"accepted","intent_id":id,"kind":"whitelist_desired","status":status,"player":name});
                files.remote_complete(tx.connection(),subject,outcome.clone())?;
                Ok(outcome)
            }).map_err(FileError::from)?;
            files.startup_whitelist.pending.store(true,Ordering::Release);
            Ok(result)
        })
    }
    pub fn startup_whitelist_receipts(&self, subject: &Subject) -> Result<Value> {
        self.db.read_with(|s| {
            self.remote_authorize(s.connection(),subject,&[(Capability::PlayersView.id().into(),String::new())])?;
            let mut query=s.connection().prepare("SELECT outcome_json FROM management_file_outcomes WHERE server_id=? AND principal_id=? AND device_id=? AND json_valid(outcome_json) AND json_extract(outcome_json,'$.kind')='whitelist_desired' ORDER BY completed_at DESC,rowid DESC LIMIT 128")?;
            let rows=query.query_map(params![self.db.server_id(),subject.principal.as_str(),subject.device.as_str()],|row|row.get::<_,String>(0))?;
            let items=rows.map(|row|{let row:Value=serde_json::from_str(&row?).map_err(|_|FileError::Corrupt)?;Ok(json!({"id":row["intent_id"],"status":row["status"],"player":row["player"]}))}).collect::<Result<Vec<_>>>()?;
            Ok(json!({"items":items}))
        }).map_err(FileError::from)
    }
    pub fn startup_whitelist_receipts_selected(
        &self,
        subject: &Subject,
        ids: &[String],
    ) -> Result<Value> {
        if ids.is_empty()
            || ids.len() > 32
            || ids.iter().collect::<std::collections::HashSet<_>>().len() != ids.len()
            || ids.iter().any(|id| uuid::Uuid::parse_str(id).is_err())
        {
            return Err(FileError::InvalidPath);
        }
        self.db.read_with(|s| {
            self.remote_authorize(s.connection(),subject,&[(Capability::PlayersView.id().into(),String::new())])?;
            let mut items=Vec::with_capacity(ids.len());
            for id in ids {
                let row:Option<String>=s.connection().query_row("SELECT outcome_json FROM management_file_outcomes WHERE server_id=? AND principal_id=? AND device_id=? AND json_valid(outcome_json) AND json_extract(outcome_json,'$.kind')='whitelist_desired' AND json_extract(outcome_json,'$.intent_id')=?",params![self.db.server_id(),subject.principal.as_str(),subject.device.as_str(),id],|r|r.get(0)).optional()?;
                let row:Value=if let Some(row)=row {serde_json::from_str(&row).map_err(|_|FileError::Corrupt)?} else {json!({"status":"unknown","player":""})};
                items.push(json!({"id":id,"status":row["status"],"player":row["player"]}));
            }
            Ok(json!({"items":items}))
        }).map_err(FileError::from)
    }
    pub fn projected_whitelist(&self, runtime: &RuntimeHandle) -> Result<Vec<WhitelistEntry>> {
        let mut actual =
            moderation::whitelist(self.root.raw_path()).map_err(|_| FileError::Corrupt)?;
        if !self.startup_whitelist.pending.load(Ordering::Acquire) {
            return Ok(actual);
        }
        let record = self
            .db
            .read_with(|s| load(s.connection(), self.db.server_id()))
            .map_err(FileError::from)?;
        let generation = runtime
            .game_snapshot()
            .map_err(|_| FileError::Unavailable)?
            .generation
            .map(|g| g.to_hex());
        for e in record
            .entries
            .iter()
            .filter(|e| generation.as_deref() == Some(e.generation.as_str()))
        {
            actual.retain(|v| !v.name.eq_ignore_ascii_case(&e.player));
            if e.add {
                actual.push(WhitelistEntry {
                    name: e.player.clone(),
                    uuid: e.uuid.clone(),
                })
            }
        }
        Ok(actual)
    }
    /// Existing host maintenance cadence; zero SQLite/file work with no desired changes.
    pub(crate) fn tick_startup_whitelist(&self, runtime: &RuntimeHandle) -> Result<Vec<String>> {
        if !self.startup_whitelist.pending.load(Ordering::Acquire) {
            return Ok(Vec::new());
        }
        let _guard = match self.startup_whitelist.lock.try_lock() {
            Ok(g) => g,
            Err(_) => return Ok(Vec::new()),
        };
        let mut record = self
            .db
            .read_with(|s| load(s.connection(), self.db.server_id()))
            .map_err(FileError::from)?;
        let snapshot = runtime
            .game_snapshot()
            .map_err(|_| FileError::Unavailable)?;
        let ready = runtime.readiness().map_err(|_| FileError::Unavailable)?;
        let now = self.db.clock().millis();
        let mut failed = Vec::new();
        let mut failed_ids = Vec::new();
        let mut changed = false;
        let mut reject = |e: &Change| {
            failed.push(e.player.clone());
            failed_ids.push(e.id.clone());
        };
        let mut superseded_ids = Vec::new();
        let mut removed = Vec::new();
        let mut persisted: Option<Option<Vec<WhitelistEntry>>> = None;
        let length = record.entries.len();
        let start = self
            .startup_whitelist
            .cursor
            .fetch_add(4, Ordering::Relaxed);
        for index in (0..length.min(4)).map(|offset| start.wrapping_add(offset) % length) {
            // Per-player order survives rotating windows and lost admission receipts.
            if record.entries[..index]
                .iter()
                .enumerate()
                .any(|(prior, e)| {
                    !removed.contains(&prior)
                        && same_player(
                            e,
                            &record.entries[index].player,
                            &record.entries[index].generation,
                        )
                })
            {
                continue;
            }
            let e = &mut record.entries[index];
            let subject = e.subject()?;
            if snapshot.generation.map(|g| g.to_hex()).as_deref() != Some(&e.generation) {
                reject(e);
                removed.push(index);
                continue;
            }
            match self.check_global_authority(&subject, Capability::PlayersModerate) {
                Ok(()) => (),
                Err(FileError::Control(_)) => {
                    reject(e);
                    removed.push(index);
                    continue;
                }
                Err(_) => continue,
            }
            if !e.dispatching {
                if snapshot.phase != crate::operations::ProcessPhase::Running {
                    reject(e);
                    removed.push(index);
                    continue;
                }
                if matches!(ready.readiness, Readiness::Degraded(_)) {
                    reject(e);
                    removed.push(index);
                    continue;
                }
                if ready.readiness != Readiness::Ready || ready.generation != snapshot.generation {
                    continue;
                }
                if online(self).ok() != Some(e.online) {
                    reject(e);
                    removed.push(index);
                    continue;
                }
                e.dispatching = true;
                e.submitted_at = Some(now);
                changed = true;
                // Persist BEFORE operation admission. A lost receipt uses the same operation key.
                self.db
                    .write_with(|tx| save(tx.connection(), &self.db, &record))
                    .map_err(FileError::from)?;
            }
            let e = &mut record.entries[index];
            if e.operation.is_none() {
                if ready.readiness != Readiness::Ready || ready.generation != snapshot.generation {
                    continue;
                }
                let generation =
                    GenerationId::parse_hex(&e.generation).map_err(|_| FileError::Corrupt)?;
                let action = if e.add {
                    ModerationAction::WhitelistAdd {
                        player: e.player.clone(),
                    }
                } else {
                    ModerationAction::WhitelistRemove {
                        player: e.player.clone(),
                    }
                };
                match runtime.submit_game(
                    &subject,
                    GameCommand::Moderate {
                        expected_generation: generation,
                        action,
                    },
                    &format!("startup-whitelist:{}", e.id),
                ) {
                    Ok(Admission::Created(op) | Admission::Existing(op)) => {
                        e.operation = Some(op.id.as_str().into());
                        changed = true;
                    }
                    Err(
                        crate::operations::OperationError::Rejected { .. }
                        | crate::operations::OperationError::Policy(_),
                    ) => {
                        reject(e);
                        removed.push(index);
                        continue;
                    }
                    Err(_) => continue,
                }
            }
            let id = OperationId::parse(e.operation.as_deref().ok_or(FileError::Corrupt)?)
                .map_err(|_| FileError::Corrupt)?;
            let op = runtime
                .operations_service()
                .whitelist_receipt(&subject, &id)
                .map_err(|_| FileError::Unavailable)?;
            if let Some(op) = op {
                if op.state == OperationState::Succeeded {
                    let superseded = record.entries[index + 1..].iter().any(|later| {
                        same_player(
                            later,
                            &record.entries[index].player,
                            &record.entries[index].generation,
                        )
                    });
                    if superseded {
                        superseded_ids.push(record.entries[index].id.clone());
                        removed.push(index);
                        continue;
                    }
                    let e = &mut record.entries[index];
                    if e.verification_since.is_none() {
                        e.verification_since = Some(now);
                        changed = true;
                    }
                    let actual = persisted
                        .get_or_insert_with(|| moderation::whitelist(self.root.raw_path()).ok());
                    let present = actual.as_ref().map(|actual| {
                        actual.iter().any(|v| {
                            uuid::Uuid::parse_str(&v.uuid).ok()
                                == uuid::Uuid::parse_str(&e.uuid).ok()
                                || (!e.add && v.name.eq_ignore_ascii_case(&e.player))
                        })
                    });
                    if present == Some(e.add) {
                        removed.push(index)
                    } else if now < e.verification_since.unwrap_or(now)
                        || now.saturating_sub(e.verification_since.unwrap_or(now)) > VERIFY_MS
                    {
                        reject(e);
                        removed.push(index)
                    }
                } else if op.state.is_terminal() || op.state == OperationState::RecoveryRequired {
                    reject(e);
                    removed.push(index)
                }
            } else {
                reject(e);
                removed.push(index)
            }
        }
        let mut terminal = Vec::new();
        if !removed.is_empty() {
            changed = true;
            removed.sort_unstable();
            removed.dedup();
            for index in removed.iter().rev() {
                terminal.push(record.entries.remove(*index));
            }
        }
        if changed {
            self.db
                .write_with(|tx| {
                    save(tx.connection(), &self.db, &record)?;
                    for e in &terminal {
                        let status = if superseded_ids.contains(&e.id) {
                            "superseded"
                        } else if failed_ids.contains(&e.id) {
                            "failed"
                        } else {
                            "applied"
                        };
                        receipt(tx.connection(), &self.db, e, status)?;
                    }
                    for player in &failed {
                        self.audit.append(
                            tx.connection(),
                            &AuditRecord::new(
                                AuditActor::new(
                                    "system".into(),
                                    "system".into(),
                                    "players".into(),
                                )?,
                                "players.whitelist.verify_failed".into(),
                                "whitelist".into(),
                                "failed".into(),
                                None,
                                [("player".into(), Some(player.clone()))],
                            )?,
                        )?;
                    }
                    Ok::<_, FileError>(())
                })
                .map_err(FileError::from)?;
        }
        self.startup_whitelist
            .pending
            .store(!record.entries.is_empty(), Ordering::Release);
        Ok(failed)
    }
}
