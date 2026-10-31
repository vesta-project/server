//! Durable content intents. Live effects reuse the file journal and its trash.
use super::*;
use crate::content::{
    ContentCapabilities, ContentChange, ContentEntry, ContentInventory, ContentKind,
    ContentMutation, ContentProvider, DeclaredMetadata, UreqProviderClient,
};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{collections::BTreeMap, io::Seek};

const MAX_PENDING: i64 = vesta_protocol::pack::catalogue::MAX_FILES as i64;
static CONTENT_ADMISSION: std::sync::Mutex<()> = std::sync::Mutex::new(());
const MAX_STAGED: i64 = 128 * 1024 * 1024;
pub(super) fn initialize(db: &StateDatabase) -> Result<()> {
    db.write(|c| c.execute_batch("CREATE TABLE IF NOT EXISTS server_content_changes (
        id TEXT PRIMARY KEY, principal TEXT NOT NULL, device TEXT NOT NULL, request_key TEXT NOT NULL,
        request_hash TEXT NOT NULL, change_json TEXT NOT NULL, bytes BLOB NOT NULL,
        state TEXT NOT NULL, error TEXT, UNIQUE(principal,device,request_key));
        CREATE TABLE IF NOT EXISTS server_content_provenance(path TEXT PRIMARY KEY, sha256 TEXT NOT NULL, provider_json TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS server_content_hashes(hash TEXT PRIMARY KEY, provider_json TEXT NOT NULL);").map_err(Into::into))?;
    Ok(())
}

#[derive(Clone, Serialize, Deserialize)]
struct Change {
    #[serde(default)]
    pack_file: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    payload: Option<crate::packs::payload::Payload>,
    #[serde(default)]
    folder: bool,
    kind: ContentKind,
    action: String,
    path: String,
    target: Option<String>,
    expected: Option<String>,
    provider: Option<ContentProvider>,
}
impl Change {
    fn authority(&self) -> Vec<(String, String)> {
        let cap = match self.action.as_str() {
            "remove" => Capability::FilesDelete,
            "enable" | "disable" => Capability::FilesRename,
            "update" => Capability::FilesOverwrite,
            _ => Capability::FilesCreate,
        };
        let mut authority = vec![
            (Capability::ContentInstall.id().into(), String::new()),
            (cap.id().into(), self.path.clone()),
        ];
        if let Some(target) = &self.target {
            authority.push((cap.id().into(), target.clone()));
        }
        authority
    }
}
impl FileBrowser {
    pub(crate) fn pack_jar_side_reader(
        reader: impl std::io::Read + std::io::Seek,
    ) -> Option<String> {
        jar_metadata(reader).ok().flatten().and_then(|m| m.side)
    }

    pub(crate) fn pack_admission(&self) -> Result<std::sync::MutexGuard<'static, ()>> {
        CONTENT_ADMISSION.lock().map_err(|_| FileError::Unavailable)
    }
    /// Insert a reviewed pack atomically with its receipt; always wait for restart.
    #[allow(clippy::type_complexity)]
    pub(crate) fn stage_pack_in(
        &self,
        c: &rusqlite::Connection,
        subject: &Subject,
        key: &str,
        files: &[(String, Option<String>, Vec<u8>, Option<ContentProvider>)],
    ) -> Result<Vec<String>> {
        self.stage_pack_bytes_in(c, subject, key, files, None)
    }
    #[allow(clippy::type_complexity)]
    pub(crate) fn stage_pack_payloads_in(
        &self,
        c: &rusqlite::Connection,
        subject: &Subject,
        key: &str,
        files: &[(
            String,
            Option<String>,
            crate::packs::payload::Payload,
            Option<ContentProvider>,
        )],
    ) -> Result<Vec<String>> {
        let rows: Vec<_> = files
            .iter()
            .map(|(p, e, _, r)| (p.clone(), e.clone(), Vec::new(), r.clone()))
            .collect();
        let payloads: Vec<_> = files.iter().map(|(_, _, b, _)| b.clone()).collect();
        self.stage_pack_bytes_in(c, subject, key, &rows, Some(&payloads))
    }
    #[allow(clippy::type_complexity)]
    fn stage_pack_bytes_in(
        &self,
        c: &rusqlite::Connection,
        subject: &Subject,
        key: &str,
        files: &[(String, Option<String>, Vec<u8>, Option<ContentProvider>)],
        payloads: Option<&[crate::packs::payload::Payload]>,
    ) -> Result<Vec<String>> {
        let (count, size): (i64, i64) = c.query_row("SELECT count(*),coalesce(sum(length(bytes)),0) FROM server_content_changes WHERE state='pending'", [], |r| Ok((r.get(0)?,r.get(1)?)))?;
        // Spooled pack plans already carry acknowledged count/size warnings.
        // Keep the interactive in-memory staging limits for other callers.
        if payloads.is_none()
            && (count + files.len() as i64 > MAX_PENDING
                || size + files.iter().map(|f| f.2.len() as i64).sum::<i64>() > MAX_STAGED)
        {
            return Err(FileError::IntentsFull);
        }
        let mut ids = Vec::new();
        for (index, (path, expected, bytes, provider)) in files.iter().enumerate() {
            let change = Change {
                payload: payloads.map(|values| values[index].clone()),
                folder: false,
                pack_file: true,
                kind: ContentKind::Mod,
                action: if expected.is_some() {
                    "update"
                } else {
                    "install"
                }
                .into(),
                path: path.clone(),
                target: None,
                expected: expected.clone(),
                provider: provider.clone(),
            };
            self.validate_content_change(&change)?;
            if bytes.len() > MAX_WRITE_BYTES {
                return Err(FileError::TooLarge);
            }
            self.remote_authorize(c, subject, &change.authority())?;
            let busy: bool = c.query_row("SELECT EXISTS(SELECT 1 FROM server_content_changes WHERE state='pending' AND json_extract(change_json,'$.path')=?)",[path],|r|r.get(0))?;
            if busy {
                return Err(FileError::Conflict(None));
            }
            let id = uuid::Uuid::new_v4().to_string();
            let raw = serde_json::to_string(&change).map_err(|_| FileError::Unavailable)?;
            c.execute(
                "INSERT INTO server_content_changes VALUES(?,?,?,?,?,?,?,'pending',NULL)",
                params![
                    id,
                    subject.principal.as_str(),
                    subject.device.as_str(),
                    format!("{key}-{index}"),
                    hex::encode(Sha256::digest([raw.as_bytes(), bytes].concat())),
                    raw,
                    bytes
                ],
            )?;
            content_audit(c, &self.audit, subject, "content.stage", &id)?;
            ids.push(id);
        }
        if !ids.is_empty() {
            self.content_barrier(true)?;
        }
        Ok(ids)
    }
    pub(crate) fn stage_pack_removals_in(
        &self,
        c: &rusqlite::Connection,
        subject: &Subject,
        key: &str,
        removals: &[(String, String)],
    ) -> Result<Vec<String>> {
        let mut ids = Vec::new();
        for (index, (path, hash)) in removals.iter().enumerate() {
            let change = Change {
                payload: None,
                pack_file: true,
                folder: false,
                kind: ContentKind::Mod,
                action: "remove".into(),
                path: path.clone(),
                target: None,
                expected: Some(hash.clone()),
                provider: None,
            };
            self.remote_authorize(c, subject, &change.authority())?;
            let batch = vec![(path.clone(), Some(hash.clone()), Vec::new(), None)];
            let staged = self.stage_pack_in(c, subject, &format!("{key}-{index}"), &batch)?;
            for id in &staged {
                c.execute(
                    "UPDATE server_content_changes SET change_json=? WHERE id=?",
                    params![
                        serde_json::to_string(&change).map_err(|_| FileError::Unavailable)?,
                        id
                    ],
                )?;
            }
            ids.extend(staged);
        }
        Ok(ids)
    }
    pub(super) fn content_tree(&self, subject: &Subject, path: &str) -> Result<(String, u64)> {
        let mut work = vec![path.to_owned()];
        let mut entries = Vec::new();
        let mut total = 0u64;
        while let Some(path) = work.pop() {
            if entries.len() + work.len() > 4000 {
                return Err(FileError::TooLarge);
            }
            for entry in self
                .list(subject, Some(&RelativePath::parse(&path)?))?
                .entries
            {
                let path = format!("{path}/{}", entry.name);
                if entry.kind == EntryKind::Directory {
                    work.push(path.clone());
                    entries.push((path, String::new()));
                } else {
                    total = total.saturating_add(entry.size);
                    if total > 64 * 1024 * 1024 {
                        return Err(FileError::TooLarge);
                    }
                    let mut stream = self.open_stream(subject, &RelativePath::parse(&path)?)?;
                    let hash = hex::encode(stream.snapshot_digest(0, stream.size())?);
                    entries.push((path, hash));
                }
            }
        }
        entries.sort();
        let raw = serde_json::to_vec(&entries).map_err(|_| FileError::Unavailable)?;
        Ok((hex::encode(Sha256::digest(raw)), total))
    }
    /// Explicit opt-in observation, maximum one 16 MiB JAR and one 5s network call.
    pub fn content_identify(
        &self,
        subject: &Subject,
        path: &str,
    ) -> Result<Option<ContentProvider>> {
        self.check_global_authority(subject, Capability::ContentInstall)?;
        if !is_jar(path) || !(path.starts_with("mods/") || path.starts_with("plugins/")) {
            return Err(FileError::InvalidPath);
        }
        let relative = RelativePath::parse(path)?;
        let mut stream = self.open_stream(subject, &relative)?;
        if stream.size() > MAX_WRITE_BYTES as u64 {
            return Err(FileError::TooLarge);
        }
        let mut sha512 = sha2::Sha512::new();
        let mut sha256 = Sha256::new();
        let mut cursor = 0;
        while cursor < stream.size() {
            let bytes = stream.read_chunk(cursor, FILE_CHUNK_BYTES)?;
            cursor += bytes.len() as u64;
            sha512.update(&bytes);
            sha256.update(&bytes);
        }
        if !stream.unchanged()? {
            return Err(FileError::Conflict(None));
        }
        let hash = hex::encode(sha512.finalize());
        let digest = hex::encode(sha256.finalize());
        let cached = self.db.read(|c| {
            c.query_row(
                "SELECT provider_json FROM server_content_hashes WHERE hash=?",
                [&hash],
                |r| r.get::<_, String>(0),
            )
            .optional()
            .map_err(Into::into)
        })?;
        let provider: Option<ContentProvider> = if let Some(raw) = cached {
            serde_json::from_str(&raw).map_err(|_| FileError::Unavailable)?
        } else {
            let found = UreqProviderClient::new(None)
                .identify_sha512(&hash)
                .map_err(|_| FileError::Unavailable)?;
            self.db.write(|c|{c.execute("DELETE FROM server_content_hashes WHERE rowid IN (SELECT rowid FROM server_content_hashes ORDER BY rowid LIMIT max(0,(SELECT count(*) FROM server_content_hashes)-1023))",[])?;c.execute("INSERT OR REPLACE INTO server_content_hashes VALUES(?,?)",params![hash,serde_json::to_string(&found).map_err(|_|vesta_state::StateError::new("content_metadata_invalid"))?])?;Ok(())})?;
            found
        };
        self.reauthorize_stream(subject, &stream)?;
        if !stream.unchanged()? {
            return Err(FileError::Conflict(None));
        }
        if let Some(provider) = &provider {
            self.db.write(|c| {
                c.execute(
                    "INSERT OR REPLACE INTO server_content_provenance VALUES(?,?,?)",
                    params![
                        path,
                        digest,
                        serde_json::to_string(provider).map_err(|_| {
                            vesta_state::StateError::new("content_metadata_invalid")
                        })?
                    ],
                )?;
                Ok(())
            })?;
        }
        Ok(provider)
    }
    pub fn content_inventory(
        &self,
        subject: &Subject,
        software: Option<String>,
        minecraft_version: String,
        world: &crate::world::WorldStatus,
    ) -> Result<ContentInventory> {
        self.check_remote_surface(subject, &[Capability::FilesList, Capability::FilesRead])?;
        let mut items = Vec::new();
        let mut truncated = false;
        let mut hash_budget = 64 * 1024 * 1024u64;
        for (kind, dir) in [
            (ContentKind::Mod, "mods"),
            (ContentKind::Plugin, "plugins"),
            (ContentKind::ResourcePack, "resourcepacks"),
            (ContentKind::Shader, "shaderpacks"),
        ] {
            let listing = match self.list(subject, Some(&RelativePath::parse(dir)?)) {
                Ok(value) => value,
                Err(FileError::NotFound) => continue,
                Err(error) => return Err(error),
            };
            for entry in listing.entries {
                if entry.kind != EntryKind::File
                    || content_path_kind(&format!("{dir}/{}", entry.name)) != Some(kind)
                {
                    continue;
                }
                if items.len() >= vesta_protocol::pack::catalogue::MAX_FILES {
                    truncated = true;
                    break;
                }
                let path = format!("{dir}/{}", entry.name);
                let relative = RelativePath::parse(&path)?;
                let mut stream = self.open_stream(subject, &relative)?;
                // Hash only a bounded amount per inventory; provenance is never attached
                // to bytes which have not been verified in this observation.
                let hash = if entry.size <= MAX_WRITE_BYTES as u64
                    && entry.size <= hash_budget
                    && items.len() < 128
                {
                    hash_budget -= entry.size;
                    Some(hex::encode(stream.snapshot_digest(0, stream.size())?))
                } else {
                    None
                };
                let provider = hash
                    .as_deref()
                    .map(|hash| self.content_provider(&path, hash))
                    .transpose()?
                    .flatten();
                // Declared side/name come from a bounded ZIP metadata read, independently
                // of the full-file hash budget used for provider provenance.
                let declared = self.content_metadata(subject, &relative).ok().flatten();
                items.push(ContentEntry {
                    modpack_owned: false,
                    kind,
                    path,
                    filename: entry.name.clone(),
                    size: entry.size.to_string(),
                    enabled: Some(!entry.name.ends_with(".disabled")),
                    sha256: hash,
                    provider,
                    declared,
                    pending: None,
                    pending_id: None,
                    pending_error: None,
                });
            }
        }
        for pack in &world.details.datapacks {
            let Some(path) = &pack.path else {
                continue;
            };
            let relative = RelativePath::parse(path)?;
            let checked = self.root.resolve(Some(&relative))?;
            let mut size = checked.metadata.len();
            let hash = if checked.metadata.is_dir() {
                match self.content_tree(subject, path) {
                    Ok((hash, bytes)) => {
                        size = bytes;
                        Some(hash)
                    }
                    Err(FileError::TooLarge) => None,
                    Err(e) => return Err(e),
                }
            } else if checked.metadata.is_file() && checked.metadata.len() <= MAX_WRITE_BYTES as u64
            {
                let mut stream = self.open_stream(subject, &relative)?;
                Some(hex::encode(stream.snapshot_digest(0, stream.size())?))
            } else {
                None
            };
            let provider = hash
                .as_deref()
                .map(|hash| self.content_provider(path, hash))
                .transpose()?
                .flatten();
            items.push(ContentEntry {
                modpack_owned: false,
                kind: ContentKind::Datapack,
                path: path.clone(),
                filename: pack.name.clone(),
                size: size.to_string(),
                enabled: pack.enabled,
                sha256: hash,
                provider,
                declared: None,
                pending: None,
                pending_id: None,
                pending_error: None,
            });
        }
        let changes = self.db.read_with(|tx| {
            let mut q = tx.connection().prepare("SELECT id,change_json,error FROM server_content_changes WHERE state='pending' ORDER BY rowid LIMIT 65536")?;
            q.query_map([], |r| Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,Option<String>>(2)?)))?.collect::<std::result::Result<Vec<_>,_>>().map_err(FileError::from)
        }).map_err(FileError::from)?;
        let mut positions: BTreeMap<_, _> = items
            .iter()
            .enumerate()
            .map(|(index, item)| (item.path.clone(), index))
            .collect();
        let deferred_writes = changes.len();
        for (id, raw, error) in changes {
            let mut change: Change =
                serde_json::from_str(&raw).map_err(|_| FileError::Unavailable)?;
            if change.pack_file {
                let Some(kind) = content_path_kind(&change.path) else {
                    continue;
                };
                change.kind = kind;
            }
            // Pending paths and provider IDs obey the same scopes as live files.
            self.validate_content_change(&change)?;
            self.db
                .read_with(|tx| {
                    self.remote_authorize(
                        tx.connection(),
                        subject,
                        &[(Capability::FilesRead.id().into(), change.path.clone())],
                    )
                })
                .map_err(FileError::from)?;
            let index = positions.get(&change.path).copied();
            let item = if let Some(index) = index {
                &mut items[index]
            } else {
                positions.insert(change.path.clone(), items.len());
                items.push(ContentEntry {
                    modpack_owned: false,
                    kind: change.kind,
                    filename: change.path.rsplit('/').next().unwrap_or_default().into(),
                    path: change.path.clone(),
                    size: change
                        .payload
                        .as_ref()
                        .map_or_else(|| "0".into(), |p| p.size.to_string()),
                    enabled: None,
                    sha256: None,
                    provider: change.provider.clone(),
                    declared: None,
                    pending: None,
                    pending_id: None,
                    pending_error: None,
                });
                items.last_mut().ok_or(FileError::Unavailable)?
            };
            if is_jar(&change.path)
                && let Some(payload) = &change.payload
            {
                let store = crate::packs::payload::Store::open(&self.db)?;
                let _lease = store.lease()?;
                item.declared = store
                    .open_file(payload)
                    .ok()
                    .and_then(|file| jar_metadata(file).ok().flatten());
            }
            item.pending = Some(change.action);
            item.pending_id = Some(id);
            item.pending_error = error;
        }
        let ownership: BTreeMap<String,String> = self.db.read(|c| {
            let exists:bool=c.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='server_modpack_files')",[],|r|r.get(0))?;
            if !exists {return Ok(BTreeMap::new())}
            let mut q=c.prepare("SELECT path,sha256 FROM server_modpack_files")?;
            q.query_map([],|r|Ok((r.get(0)?,r.get(1)?)))?.collect::<std::result::Result<_,_>>().map_err(Into::into)
        })?;
        for item in &mut items {
            item.modpack_owned = ownership.contains_key(&item.path);
        }
        let policy = self
            .db
            .read_with(|tx| {
                identity::policy_in(
                    tx.connection(),
                    self.db.server_id(),
                    self.db.clock().millis(),
                    Some(subject),
                )
                .map_err(FileError::from)
            })
            .map_err(FileError::from)?;
        let content = policy.check(Capability::ContentInstall, None).allowed();
        let has = |cap| policy.check(cap, None).allowed();
        Ok(ContentInventory {
            deferred_writes,
            needs_restart: deferred_writes > 0,
            items,
            minecraft_version,
            truncated,
            capabilities: ContentCapabilities {
                list: true,
                install: content && has(Capability::FilesCreate),
                update: content && has(Capability::FilesOverwrite),
                remove: content && has(Capability::FilesDelete),
                toggle: content && has(Capability::FilesRename),
                cancel: content,
                datapack_toggle_offline: has(Capability::ConfigEdit)
                    && has(Capability::FilesOverwrite),
                datapack_toggle_running: has(Capability::ConfigEdit)
                    && has(Capability::ConsoleWrite),
                mods_supported: matches!(
                    software.as_deref(),
                    Some("fabric" | "quilt" | "forge" | "neoforge")
                ),
                plugins_supported: matches!(
                    software.as_deref(),
                    Some("paper" | "spigot" | "bukkit" | "purpur" | "folia")
                ),
            },
            software,
        })
    }
    pub(crate) fn content_provider(
        &self,
        path: &str,
        hash: &str,
    ) -> Result<Option<ContentProvider>> {
        let raw = self.db.read(|c| {
            c.query_row(
                "SELECT provider_json FROM server_content_provenance WHERE path=? AND sha256=?",
                params![path, hash],
                |r| r.get::<_, String>(0),
            )
            .optional()
            .map_err(Into::into)
        })?;
        raw.map(|raw| serde_json::from_str(&raw).map_err(|_| FileError::Unavailable))
            .transpose()
    }
    pub fn content_metadata(
        &self,
        subject: &Subject,
        path: &RelativePath,
    ) -> Result<Option<DeclaredMetadata>> {
        // Admission checks all submitted/canonical/handle scopes before ZIP parsing.
        let _stream = self.open_stream(subject, path)?;
        let checked = self.root.resolve(Some(path))?;
        if checked.metadata.len() > 256 * 1024 * 1024 {
            return Err(FileError::TooLarge);
        }
        let file = native::open_regular(&checked.path)?;
        jar_metadata(file)
    }
    pub fn content_mutate(
        &self,
        runtime: &crate::operations::RuntimeHandle,
        subject: &Subject,
        key: &str,
        mutation: ContentMutation,
    ) -> Result<ContentChange> {
        let (action, kind, path, expected) = match mutation {
            ContentMutation::Remove {
                kind,
                path,
                expected_sha256,
            } => ("remove", kind, path, expected_sha256),
            ContentMutation::Enable {
                kind,
                path,
                expected_sha256,
            } => ("enable", kind, path, expected_sha256),
            ContentMutation::Disable {
                kind,
                path,
                expected_sha256,
            } => ("disable", kind, path, expected_sha256),
        };
        let target = match action {
            "enable" => Some(
                path.strip_suffix(".disabled")
                    .ok_or(FileError::InvalidPath)?
                    .into(),
            ),
            "disable" if !path.ends_with(".disabled") => Some(format!("{path}.disabled")),
            "disable" => return Err(FileError::InvalidPath),
            _ => None,
        };
        if kind == ContentKind::Datapack && action != "remove" {
            return Err(FileError::InvalidPath);
        }
        let folder = if kind == ContentKind::Datapack && action == "remove" {
            self.db
                .read_with(|tx| {
                    self.remote_authorize(
                        tx.connection(),
                        subject,
                        &[(Capability::FilesDelete.id().into(), path.clone())],
                    )
                })
                .map_err(FileError::from)?;
            self.root
                .resolve(Some(&RelativePath::parse(&path)?))?
                .metadata
                .is_dir()
        } else {
            false
        };
        let target = if folder {
            Some(format!(
                "content-trash/{}",
                hex::encode(Sha256::digest(format!(
                    "{}:{}:{key}",
                    subject.principal.as_str(),
                    subject.device.as_str()
                )))
            ))
        } else {
            target
        };
        self.queue_content(
            runtime,
            subject,
            key,
            Change {
                payload: None,
                pack_file: false,
                folder,
                kind,
                action: action.into(),
                path,
                target,
                expected: Some(expected),
                provider: None,
            },
            &[],
        )
    }
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn content_install_bytes(
        &self,
        runtime: &crate::operations::RuntimeHandle,
        subject: &Subject,
        key: &str,
        kind: ContentKind,
        path: String,
        expected: Option<String>,
        provider: ContentProvider,
        bytes: &[u8],
    ) -> Result<ContentChange> {
        self.queue_content(
            runtime,
            subject,
            key,
            Change {
                payload: None,
                pack_file: false,
                folder: false,
                kind,
                action: if expected.is_some() {
                    "update"
                } else {
                    "install"
                }
                .into(),
                path,
                target: None,
                expected,
                provider: Some(provider),
            },
            bytes,
        )
    }
    fn validate_content_change(&self, change: &Change) -> Result<()> {
        if change.payload.is_some()
            && (!change.pack_file || !matches!(change.action.as_str(), "install" | "update"))
        {
            return Err(FileError::InvalidPath);
        }
        RelativePath::parse(&change.path)?;
        if change.pack_file {
            crate::packs::workspace::validate_install_path(&change.path)
                .map_err(|_| FileError::InvalidPath)?;
            return Ok(());
        }
        let valid = match change.kind {
            ContentKind::Mod => {
                change.path.starts_with("mods/")
                    && change.path.matches('/').count() == 1
                    && is_jar(&change.path)
            }
            ContentKind::Plugin => {
                change.path.starts_with("plugins/")
                    && change.path.matches('/').count() == 1
                    && is_jar(&change.path)
            }
            ContentKind::ResourcePack | ContentKind::Shader => {
                content_path_kind(&change.path) == Some(change.kind)
            }
            ContentKind::Datapack => {
                change.path.matches('/').count() == 2
                    && change.path.contains("/datapacks/")
                    && (change.path.ends_with(".zip")
                        || (change.folder && change.action == "remove"))
            }
        };
        if !valid {
            return Err(FileError::InvalidPath);
        }
        if let Some(hash) = &change.expected {
            mutation::check_hex(hash)?;
        }
        Ok(())
    }
    fn queue_content(
        &self,
        runtime: &crate::operations::RuntimeHandle,
        subject: &Subject,
        key: &str,
        change: Change,
        bytes: &[u8],
    ) -> Result<ContentChange> {
        let guard = StoppedGuard::acquire(runtime, self.db.directory()).ok();
        let _admission = CONTENT_ADMISSION
            .lock()
            .map_err(|_| FileError::Unavailable)?;
        self.validate_content_change(&change)?;
        if key.is_empty() || key.len() > 128 || bytes.len() > MAX_WRITE_BYTES {
            return Err(FileError::InvalidPath);
        }
        let raw = serde_json::to_string(&change).map_err(|_| FileError::Unavailable)?;
        let hash = hex::encode(Sha256::digest([raw.as_bytes(), bytes].concat()));
        let id = self.db.write_with(|tx| {
            let c = tx.connection();
            self.remote_authorize(c, subject, &change.authority())?;
            if let Some((id, stored)) = c.query_row("SELECT id,request_hash FROM server_content_changes WHERE principal=? AND device=? AND request_key=?",params![subject.principal.as_str(),subject.device.as_str(),key],|r| Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?))).optional()? {
                return if hash == stored { Ok(id) } else { Err(FileError::IdempotencyConflict) };
            }
            let (count,size): (i64,i64) = c.query_row("SELECT count(*),COALESCE(sum(length(bytes)),0) FROM server_content_changes WHERE state='pending'",[],|r| Ok((r.get(0)?,r.get(1)?)))?;
            let retained:i64=c.query_row("SELECT count(*) FROM server_content_changes WHERE coalesce(json_extract(change_json,'$.pack_file'),0)=0",[],|r|r.get(0))?;
            if retained>=10000{return Err(FileError::IntentsFull);}
            if count >= MAX_PENDING || size + bytes.len() as i64 > MAX_STAGED { return Err(FileError::IntentsFull); }
            let busy: bool = c.query_row("SELECT EXISTS(SELECT 1 FROM server_content_changes WHERE state='pending' AND (json_extract(change_json,'$.path') IN (?,?) OR json_extract(change_json,'$.target') IN (?,?)))",params![change.path,change.target,change.path,change.target],|r|r.get(0))?;
            if busy { return Err(FileError::Conflict(None)); }
            // The marker is durable before admission. A crash can leave a harmless
            // extra barrier; the next agent start reconciles it from this table.
            self.content_barrier(true)?;
            let id = uuid::Uuid::new_v4().to_string();
            c.execute("INSERT INTO server_content_changes VALUES(?,?,?,?,?,?,?,'pending',NULL)",params![id,subject.principal.as_str(),subject.device.as_str(),key,hash,raw,bytes])?;
            content_audit(c, &self.audit, subject, "content.stage", &id)?;
            Ok(id)
        }).map_err(FileError::from)?;
        if let Some(guard) = guard {
            guard.check()?;
            self.apply_content_id(&id)?;
        }
        self.content_change(&id)
    }
    fn content_change(&self, id: &str) -> Result<ContentChange> {
        self.db
            .read_with(|tx| {
                let (raw, state, error): (String, String, Option<String>) =
                    tx.connection().query_row(
                        "SELECT change_json,state,error FROM server_content_changes WHERE id=?",
                        [id],
                        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                    )?;
                let change: Change =
                    serde_json::from_str(&raw).map_err(|_| FileError::Unavailable)?;
                Ok(ContentChange {
                    id: id.into(),
                    trash_path: if change.folder && state == "applied" {
                        change.target
                    } else {
                        None
                    },
                    pending: (state == "pending").then_some(change.action),
                    state,
                    path: change.path,
                    error,
                })
            })
            .map_err(FileError::from)
    }
    pub fn content_cancel(&self, subject: &Subject, id: &str) -> Result<ContentChange> {
        let _admission = CONTENT_ADMISSION
            .lock()
            .map_err(|_| FileError::Unavailable)?;
        self.content_cancel_inner(subject, id)
    }
    fn content_cancel_inner(&self, subject: &Subject, id: &str) -> Result<ContentChange> {
        self.db
            .write_with(|tx| {
                let c = tx.connection();
                self.remote_authorize(
                    c,
                    subject,
                    &[(Capability::ContentInstall.id().into(), String::new())],
                )?;
                let actor: (String, String, String) = c
                    .query_row(
                        "SELECT principal,device,state FROM server_content_changes WHERE id=?",
                        [id],
                        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                    )
                    .optional()?
                    .ok_or(FileError::NotFound)?;
                if actor.0 != subject.principal.as_str() || actor.1 != subject.device.as_str() {
                    let policy=identity::policy_in(c,self.db.server_id(),self.db.clock().millis(),Some(subject))?;
                    if !policy.holds_builtin(identity::BuiltinRole::Owner){return Err(ControlError::forbidden(identity::DenyReason::NotGranted).into());}
                }
                if actor.2 == "applied" {
                    return Err(FileError::Conflict(None));
                }
                let started:bool=c.query_row("SELECT EXISTS(SELECT 1 FROM management_file_outcomes WHERE request_key=?)",[format!("content-{id}")],|r|r.get(0))?;
                if started {return Err(FileError::Conflict(None));}
                c.execute(
                    "UPDATE server_content_changes SET state='cancelled',bytes=X'' WHERE id=?",
                    [id],
                )?;
                content_audit(c, &self.audit, subject, "content.cancel", id)?;
                let path:String=c.query_row("SELECT json_extract(change_json,'$.path') FROM server_content_changes WHERE id=?",[id],|r|r.get(0))?;
                self.remote_complete(c,subject,json!({"id":id,"state":"cancelled","pending":null,"path":path,"error":null,"trash_path":null}))
            })
            .map_err(FileError::from)?;
        self.reconcile_content_barrier()?;
        let _ = crate::packs::payload::Store::open(&self.db).and_then(|store| store.collect());
        self.content_change(id)
    }
    pub fn content_cancel_remote(
        &self,
        subject: &Subject,
        id: &str,
        key: &str,
    ) -> Result<serde_json::Value> {
        let _admission = CONTENT_ADMISSION
            .lock()
            .map_err(|_| FileError::Unavailable)?;
        self.for_remote().execute_remote(
            subject,
            key,
            json!(["content-cancel", id]),
            || {
                Ok(vec![(
                    Capability::ContentInstall.id().into(),
                    String::new(),
                )])
            },
            |files| {
                serde_json::to_value(files.content_cancel_inner(subject, id)?)
                    .map_err(|_| FileError::Unavailable)
            },
        )
    }
    fn content_barrier(&self, pending: bool) -> Result<()> {
        let path = self.db.directory().path().join("content-pending");
        if pending {
            match fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(file) => {
                    self.db.directory().tighten_file(&path)?;
                    file.sync_all().map_err(io_error)?;
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    super::read_local_document(&path, 1)?;
                }
                Err(error) => return Err(io_error(error)),
            }
        } else if let Err(error) = fs::remove_file(path)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            return Err(io_error(error));
        }
        #[cfg(unix)]
        fs::File::open(self.db.directory().path())
            .and_then(|dir| dir.sync_all())
            .map_err(io_error)?;
        Ok(())
    }
    pub(crate) fn reconcile_content_barrier(&self) -> Result<()> {
        let count = self.db.read(|c| {
            c.query_row(
                "SELECT count(*) FROM server_content_changes WHERE state='pending'",
                [],
                |r| r.get::<_, i64>(0),
            )
            .map_err(Into::into)
        })?;
        self.content_barrier(count != 0)
    }
    /// Called with sole stopped process ownership, before every agent start/restart.
    /// Supervisor starts are independently barred by the protected marker.
    pub(crate) fn apply_pending_content(&self) -> Result<()> {
        let _admission = CONTENT_ADMISSION
            .lock()
            .map_err(|_| FileError::Unavailable)?;
        loop {
            let ids = self.db.read(|c| { let mut q=c.prepare("SELECT id FROM server_content_changes WHERE state='pending' ORDER BY rowid LIMIT 64")?; q.query_map([],|r|r.get::<_,String>(0))?.collect::<std::result::Result<Vec<_>,_>>().map_err(Into::into) })?;
            if ids.is_empty() {
                break;
            }
            for id in ids {
                self.apply_content_id(&id)?;
            }
        }
        let _ = crate::packs::payload::Store::open(&self.db).and_then(|store| store.collect());
        self.reconcile_content_barrier()
    }
    /// Apply only this install's journal entries while stopped ownership is held.
    pub(crate) fn apply_pack_ids(&self, ids: &[String]) -> Result<()> {
        for id in ids { self.apply_content_id(id)?; }
        self.reconcile_content_barrier()
    }
    fn apply_content_id(&self, id: &str) -> Result<()> {
        let row = self.db.read(|c|c.query_row("SELECT principal,device,change_json,bytes FROM server_content_changes WHERE id=? AND state='pending'",[id],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,Vec<u8>>(3)?))).optional().map_err(Into::into))?;
        let Some((principal, device, raw, bytes)) = row else {
            return Ok(());
        };
        let subject = Subject {
            principal: identity::PrincipalId::parse(&principal)
                .map_err(|_| FileError::Unavailable)?,
            device: identity::DeviceId::parse(&device).map_err(|_| FileError::Unavailable)?,
        };
        let change: Change = serde_json::from_str(&raw).map_err(|_| FileError::Unavailable)?;
        self.validate_content_change(&change)?;
        let mut files = self.for_remote();
        files.pack_effect = change.pack_file;
        let path = RelativePath::parse(&change.path)?;
        let key = format!("content-{id}");
        if change.folder {
            let trash = RelativePath::parse("content-trash")?;
            if matches!(self.list(&subject, Some(&trash)), Err(FileError::NotFound)) {
                self.mkdir(&subject, &trash)?;
            }
        }
        if change.pack_file {
            let parts: Vec<_> = change.path.split('/').collect();
            for n in 1..parts.len() {
                let parent = RelativePath::parse(&parts[..n].join("/"))?;
                if matches!(self.list(&subject, Some(&parent)), Err(FileError::NotFound)) {
                    self.mkdir(&subject, &parent)?;
                }
            }
        }
        let old =
            self.content_provider(&change.path, change.expected.as_deref().unwrap_or_default())?;
        let result = files.execute_remote(&subject,&key,json!(["content",id]),||Ok(change.authority()),|files| {
            let outcome = match change.action.as_str() {
                "install" | "update" => {
                    let condition=change.expected.clone().map_or(WritePrecondition::Absent,WritePrecondition::Matches);
                    let value=if let Some(payload)=&change.payload { files.write_pack_payload(&subject,&path,&condition,payload)? } else { files.write(&subject,&path,&condition,&bytes)? };
                    json!({"sha256":value.sha256,"size":value.size.to_string(),"trash_id":value.trash})
                }
                "remove" if change.folder => {
                    let target=RelativePath::parse(change.target.as_deref().ok_or(FileError::InvalidPath)?)?;
                    files.rename_content_directory(&subject,&path,&target,change.expected.as_deref().ok_or(FileError::InvalidPath)?)?;
                    json!({"trash_path":target.as_str()})
                }
                "remove" => json!({"trash_id":files.delete(&subject,&path,change.expected.as_deref().ok_or(FileError::InvalidPath)?)?.id}),
                "enable" | "disable" => {
                    let value=files.rename(&subject,&path,&RelativePath::parse(change.target.as_deref().ok_or(FileError::InvalidPath)?)?,change.expected.as_deref().ok_or(FileError::InvalidPath)?)?;
                    json!({"sha256":value.sha256,"size":value.size.to_string()})
                }
                _ => return Err(FileError::InvalidPath),
            };
            // Mutations complete their file receipt in the same journal transaction.
            Ok(outcome)
        });
        match result {
            Ok(_) => self.db.write_with(|tx| {
                let c=tx.connection();
                c.execute("DELETE FROM server_content_provenance WHERE path=?",[&change.path])?;
                if let Some(provider)=change.provider.as_ref().or(if change.pack_file {None}else{old.as_ref()}) && change.action!="remove" {
                    let hash=if let Some(payload)=&change.payload {payload.sha256.clone()} else if bytes.is_empty(){change.expected.clone().unwrap_or_default()}else{hex::encode(Sha256::digest(&bytes))};
                    c.execute("INSERT OR REPLACE INTO server_content_provenance VALUES(?,?,?)",params![change.target.as_ref().unwrap_or(&change.path),hash,serde_json::to_string(provider).map_err(|_|FileError::Unavailable)?])?;
                }
                c.execute("UPDATE server_content_changes SET state='applied',bytes=X'',error=NULL WHERE id=?",[id])?;
                content_audit(c,&self.audit,&subject,"content.apply",id)
            }).map_err(FileError::from)?,
            Err(error) => {
                self.db.write(|c|c.execute("UPDATE server_content_changes SET error=? WHERE id=?",params![error.code(),id]).map(|_|()).map_err(Into::into))?;
                return Err(error);
            }
        }
        self.reconcile_content_barrier()
    }
}
fn content_audit(
    c: &rusqlite::Connection,
    log: &vesta_state::audit::AuditLog,
    subject: &Subject,
    action: &str,
    id: &str,
) -> Result<()> {
    use vesta_state::audit::{AuditActor, AuditRecord};
    log.append(
        c,
        &AuditRecord::new(
            AuditActor::new(
                subject.principal.as_str().into(),
                subject.device.as_str().into(),
                "content".into(),
            )?,
            action.into(),
            format!("content:{id}"),
            "succeeded".into(),
            None,
            [],
        )?,
    )?;
    Ok(())
}
fn is_jar(name: &str) -> bool {
    name.ends_with(".jar") || name.ends_with(".jar.disabled")
}

fn jar_metadata(reader: impl Read + Seek) -> Result<Option<DeclaredMetadata>> {
    let mut zip = zip::ZipArchive::new(reader).map_err(|_| FileError::Corrupt)?;
    if zip.len() > 4096 {
        return Err(FileError::TooLarge);
    }
    for (name, loader) in [
        ("fabric.mod.json", "fabric"),
        ("quilt.mod.json", "quilt"),
        ("META-INF/mods.toml", "forge"),
        ("META-INF/neoforge.mods.toml", "neoforge"),
        ("paper-plugin.yml", "paper"),
        ("plugin.yml", "bukkit"),
    ] {
        let mut file = match zip.by_name(name) {
            Ok(file) => file,
            Err(_) => continue,
        };
        if file.size() > 65536 {
            continue;
        }
        let mut text = String::new();
        file.by_ref()
            .take(65537)
            .read_to_string(&mut text)
            .map_err(io_error)?;
        if text.len() > 65536 {
            continue;
        }
        let mut result = DeclaredMetadata {
            loader: Some(loader.into()),
            ..Default::default()
        };
        if name.ends_with(".json") {
            let value: serde_json::Value =
                serde_json::from_str(&text).map_err(|_| FileError::Corrupt)?;
            let data = if loader == "quilt" {
                &value["quilt_loader"]
            } else {
                &value
            };
            let meta = if loader == "quilt" {
                &data["metadata"]
            } else {
                data
            };
            result.name = meta["name"]
                .as_str()
                .or(data["id"].as_str())
                .map(str::to_owned);
            result.version = data["version"].as_str().map(str::to_owned);
            result.side = value["environment"]
                .as_str()
                .or(value["minecraft"]["environment"].as_str())
                .map(str::to_owned);
            // Fabric defines an omitted environment as all physical environments.
            // https://docs.fabricmc.net/develop/loader/fabric-mod-json#environment
            if loader == "fabric" && value.get("environment").is_none() {
                result.side = Some("*".into());
            }
            if let Some(deps) = data["depends"].as_object() {
                result.dependencies = deps.keys().take(256).cloned().collect();
            }
            if let Some(deps) = data["depends"].as_array() {
                result.dependencies = deps
                    .iter()
                    .filter_map(|d| d["id"].as_str().map(str::to_owned))
                    .take(256)
                    .collect();
            }
        } else if name.ends_with(".toml") {
            let value: toml::Value = toml::from_str(&text).map_err(|_| FileError::Corrupt)?;
            if let Some(first) = value
                .get("mods")
                .and_then(toml::Value::as_array)
                .and_then(|v| v.first())
            {
                result.name = first
                    .get("displayName")
                    .or(first.get("modId"))
                    .and_then(toml::Value::as_str)
                    .map(str::to_owned);
                result.version = first
                    .get("version")
                    .and_then(toml::Value::as_str)
                    .map(str::to_owned);
            }
            if let Some(groups) = value.get("dependencies").and_then(toml::Value::as_table) {
                for group in groups.values() {
                    if let Some(deps) = group.as_array() {
                        for dep in deps.iter().take(256) {
                            if let Some(id) = dep.get("modId").and_then(toml::Value::as_str) {
                                result.dependencies.push(id.into());
                            }
                        }
                    }
                }
            }
        } else {
            // Bounded scalar YAML subset. Unknown/complex YAML stays unidentified.
            let values: BTreeMap<_, _> = text
                .lines()
                .filter(|line| !line.starts_with(char::is_whitespace))
                .filter_map(|line| line.split_once(':'))
                .map(|(k, v)| (k.trim(), v.trim().trim_matches(['\'', '"'])))
                .collect();
            result.name = values.get("name").map(|v| (*v).into());
            result.version = values.get("version").map(|v| (*v).into());
            result.side = Some("server".into());
            for key in ["depend", "softdepend"] {
                if let Some(value) = values.get(key) {
                    result.dependencies.extend(
                        value
                            .trim_matches(['[', ']'])
                            .split(',')
                            .map(str::trim)
                            .filter(|s| !s.is_empty())
                            .take(256)
                            .map(str::to_owned),
                    );
                }
            }
        }
        result.name = result.name.filter(|v| v.len() <= 256);
        result.version = result.version.filter(|v| v.len() <= 256);
        result.side = result.side.filter(|v| v.len() <= 64);
        result.dependencies.retain(|v| v.len() <= 256);
        result.dependencies.truncate(128);
        return Ok(Some(result));
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Cursor, Write};
    fn jar(name: &str, text: &str) -> Vec<u8> {
        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        zip.start_file(name, zip::write::SimpleFileOptions::default())
            .unwrap();
        zip.write_all(text.as_bytes()).unwrap();
        zip.finish().unwrap().into_inner()
    }
    #[test]
    fn fabric_environment_defaults_to_both_without_overriding_explicit_client_side() {
        for (document, side) in [
            (r#"{"id":"example","version":"1"}"#, "*"),
            (
                r#"{"id":"example","version":"1","environment":"client"}"#,
                "client",
            ),
        ] {
            assert_eq!(
                jar_metadata(Cursor::new(jar("fabric.mod.json", document)))
                    .unwrap()
                    .unwrap()
                    .side
                    .as_deref(),
                Some(side)
            );
        }
    }
    #[test]
    fn jar_metadata_reads_fabric_quilt_forge_and_plugin_declarations() {
        for (name, text, loader) in [
            (
                "fabric.mod.json",
                r#"{"id":"test","name":"Test","version":"1","environment":"server","depends":{"fabricloader":"*"}}"#,
                "fabric",
            ),
            (
                "quilt.mod.json",
                r#"{"quilt_loader":{"id":"test","version":"1","metadata":{"name":"Test"},"depends":[{"id":"quilt_loader"}]}}"#,
                "quilt",
            ),
            (
                "META-INF/mods.toml",
                "[[mods]]\nmodId='test'\ndisplayName='Test'\nversion='1'\n[[dependencies.test]]\nmodId='minecraft'\n",
                "forge",
            ),
            (
                "plugin.yml",
                "name: Test\nversion: '1'\ndepend: [ProtocolLib]\n",
                "bukkit",
            ),
            ("paper-plugin.yml", "name: Test\nversion: '1'\n", "paper"),
        ] {
            let metadata = jar_metadata(Cursor::new(jar(name, text))).unwrap().unwrap();
            assert_eq!(metadata.loader.as_deref(), Some(loader));
            assert_eq!(metadata.name.as_deref(), Some("Test"));
            assert_eq!(metadata.version.as_deref(), Some("1"));
        }
    }
    #[test]
    fn unknown_and_oversized_metadata_stays_unidentified() {
        assert!(
            jar_metadata(Cursor::new(jar("unrelated.txt", "test")))
                .unwrap()
                .is_none()
        );
        assert!(
            jar_metadata(Cursor::new(jar("fabric.mod.json", &"a".repeat(65537))))
                .unwrap()
                .is_none()
        );
    }
}

/// Content archives use the instance browser's folder and extension rules.
pub(crate) fn content_path_kind(path: &str) -> Option<ContentKind> {
    let path = path.to_ascii_lowercase();
    let path = path.strip_suffix(".disabled").unwrap_or(&path);
    let (folder, name) = path.rsplit_once('/')?;
    match folder {
        "mods" if name.ends_with(".jar") => Some(ContentKind::Mod),
        "plugins" if name.ends_with(".jar") => Some(ContentKind::Plugin),
        "resourcepacks" if name.ends_with(".zip") => Some(ContentKind::ResourcePack),
        "shaderpacks" if name.ends_with(".zip") => Some(ContentKind::Shader),
        _ if (folder == "datapacks" || folder.ends_with("/datapacks"))
            && name.ends_with(".zip") =>
        {
            Some(ContentKind::Datapack)
        }
        _ => None,
    }
}
#[cfg(test)]
mod content_path_tests {
    use super::*;
    #[test]
    fn content_kinds_exclude_documents_and_configuration() {
        for extension in ["md", "pdf", "cfg", "json5"] {
            for folder in [
                "mods",
                "plugins",
                "resourcepacks",
                "shaderpacks",
                "world/datapacks",
            ] {
                assert_eq!(
                    content_path_kind(&format!("{folder}/License.{extension}")),
                    None
                );
            }
        }
        assert_eq!(
            content_path_kind("mods/MOD.JAR.disabled"),
            Some(ContentKind::Mod)
        );
        assert_eq!(
            content_path_kind("resourcepacks/pack.zip"),
            Some(ContentKind::ResourcePack)
        );
        assert_eq!(
            content_path_kind("shaderpacks/pack.zip"),
            Some(ContentKind::Shader)
        );
        assert_eq!(
            content_path_kind("world/datapacks/pack.zip"),
            Some(ContentKind::Datapack)
        );
        assert_eq!(content_path_kind("config/archive.zip"), None);
    }
}
