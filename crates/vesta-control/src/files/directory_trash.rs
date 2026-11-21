//! Journal the directory move before touching either tree. Recovery observes
//! the unique private path and never repeats the move.
use super::*;
use serde_json::json;

impl FileBrowser {
    fn directory_copy(&self, id: &str, create: bool) -> Result<PathBuf> {
        trash_name(id)?;
        let root = self.db.directory().path().join("trash-folders");
        if create {
            match fs::create_dir(&root) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(io_error(e)),
            }
        }
        super::super::root::plain_ancestors(&root)?;
        Ok(fs::canonicalize(root).map_err(io_error)?.join(id))
    }

    /// Validate every relative and native spelling before moving a subtree.
    /// Only metadata is needed: rename retains the same native objects.
    fn scan_trash_tree(
        &self,
        c: &Connection,
        subject: &Subject,
        path: &RelativePath,
        tree: &Path,
        restoring: bool,
    ) -> Result<(String, u64)> {
        let policy = identity::policy_in(
            c,
            self.db.server_id(),
            self.db.clock().millis(),
            Some(subject),
        )?;
        let destination = restoring
            .then(|| self.parent(path).map(|parent| parent.join(leaf(path))))
            .transpose()?;
        #[cfg(unix)]
        let device = {
            use std::os::unix::fs::MetadataExt;
            fs::symlink_metadata(tree).map_err(io_error)?.dev()
        };
        let mut entries = vec![(path.clone(), tree.to_path_buf())];
        let mut records = Vec::new();
        let mut size = 0u64;
        while let Some((relative, native_path)) = entries.pop() {
            let metadata = if restoring {
                fs::symlink_metadata(&native_path).map_err(io_error)?
            } else {
                let checked = self.root.resolve(Some(&relative))?;
                if checked.path != native_path {
                    return Err(FileError::LinkedPath);
                }
                checked.metadata
            };
            super::super::root::plain(&metadata)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                if metadata.dev() != device {
                    return Err(FileError::LinkedPath);
                }
            }
            let proposed = if let Some(destination) = &destination {
                destination.join(
                    relative
                        .as_str()
                        .strip_prefix(path.as_str())
                        .ok_or(FileError::InvalidPath)?
                        .trim_start_matches('/'),
                )
            } else {
                native_path.clone()
            };
            let resource = self.root.resource(&proposed)?;
            for resource in [relative.resource(), resource] {
                if restoring {
                    super::super::authorize(&policy, Capability::FilesRead, &resource)?;
                    super::super::authorize(&policy, Capability::FilesCreate, &resource)?;
                } else {
                    super::super::authorize(&policy, Capability::FilesDelete, &resource)?;
                }
            }
            let directory = metadata.is_dir();
            if directory {
                for child in fs::read_dir(&native_path).map_err(io_error)? {
                    if records.len() + entries.len() >= super::super::MAX_DIRECTORY_SCAN {
                        return Err(FileError::TooLarge);
                    }
                    let child = child.map_err(io_error)?;
                    let name = child
                        .file_name()
                        .into_string()
                        .map_err(|_| FileError::InvalidPath)?;
                    entries.push((
                        RelativePath::parse(&format!("{}/{name}", relative.as_str()))?,
                        child.path(),
                    ));
                }
            } else {
                let file = native::open_regular(&native_path)?;
                if native::actual_path(&file, &native_path)? != native_path {
                    return Err(FileError::LinkedPath);
                }
                size = size
                    .checked_add(metadata.len())
                    .ok_or(FileError::TooLarge)?;
            }
            let suffix = relative
                .as_str()
                .strip_prefix(path.as_str())
                .ok_or(FileError::InvalidPath)?;
            records.push((
                suffix.to_owned(),
                directory,
                if directory { 0 } else { metadata.len() },
            ));
        }
        records.sort_unstable();
        let bytes = serde_json::to_vec(&records).map_err(|_| FileError::Unavailable)?;
        Ok((hex::encode(Sha256::digest(bytes)), size))
    }

    pub(in crate::files) fn delete_directory(
        &self,
        subject: &Subject,
        path: &RelativePath,
    ) -> Result<TrashEntry> {
        let admission = mutation_admission(self.db.directory().path())?;
        let _guard = self.mutation_guard(&admission)?;
        let id = random_id()?;
        let copy = self.directory_copy(&id, true)?;
        let entry = self.db.write_with(|tx| {
            let c = tx.connection();
            let now = self.authorize_destination(c, subject, path, Capability::FilesDelete)?;
            self.before_remote_write(c, subject)?;
            let source = self.root.resolve(Some(path))?;
            if !source.metadata.is_dir() {
                return Err(FileError::NotDirectory);
            }
            if !native::same_volume(&source.path, copy.parent().ok_or(FileError::InvalidPath)?)? {
                return Err(FileError::Unavailable);
            }
            let (sha256, size) = self.scan_trash_tree(c, subject, path, &source.path, false)?;
            self.require_trash_capacity(c, size)?;
            let expires_at = now.saturating_add(i64::try_from(self.trash_policy.retention.as_millis()).map_err(|_| FileError::InvalidLimit)?);
            let identity = native::directory_identity(&source.path)?;
            c.execute("INSERT INTO file_trash
                (id,server_id,path,sha256,size,reason,principal_id,device_id,status,readonly,trashed_at,expires_at,directory,batch_id,directory_identity)
                VALUES (?,?,?,?,?,'delete',?,?,'pending',0,?,?,1,?,?)",
                params![id,self.db.server_id(),source.resource.as_str(),sha256,i64::try_from(size).map_err(|_| FileError::TooLarge)?,subject.principal.as_str(),subject.device.as_str(),now,expires_at,self.trash_batch,identity])?;
            load_entry(c, self.db.server_id(), &id)?.ok_or(FileError::Unavailable)
        }).map_err(FileError::from)?;
        let mut moved = false;
        let result = self
            .db
            .write_with(|tx| {
                let c = tx.connection();
                self.before_remote_write(c, subject)?;
                self.authorize_destination(c, subject, path, Capability::FilesDelete)?;
                let source = self.root.resolve(Some(path))?;
                if !source.metadata.is_dir()
                    || self
                        .scan_trash_tree(c, subject, path, &source.path, false)?
                        .0
                        != entry.sha256
                {
                    return Err(FileError::Conflict(None));
                }
                native::rename_absent(&source.path, &copy)?;
                moved = true;
                self.audit(
                    c,
                    subject,
                    "file.delete",
                    path,
                    [("trash", Some(id.clone()))],
                )?;
                c.execute("UPDATE file_trash SET status='complete' WHERE id=?", [&id])?;
                Ok(entry)
            })
            .map_err(FileError::from);
        if result.is_err() && !moved {
            self.db.write(|c| {
                c.execute(
                    "DELETE FROM file_trash WHERE id=? AND status='pending'",
                    [&id],
                )?;
                Ok(())
            })?;
        }
        if moved {
            result.map_err(|e| FileError::RecoveryRequired(Box::new(e)))
        } else {
            result
        }
    }

    pub(super) fn restore_directory(
        &self,
        subject: &Subject,
        entry: &TrashEntry,
    ) -> Result<FileChange> {
        let id = &entry.id;
        let copy = self.directory_copy(id, false)?;
        let path = &entry.path;
        self.db
            .write_with(|tx| {
                let c = tx.connection();
                self.check_remote_trash_owner(c, subject, id)?;
                self.authorize_destination(c, subject, path, Capability::FilesCreate)?;
                require_absent(&self.parent(path)?.join(leaf(path)))?;
                if c.execute(
                    "UPDATE file_trash SET status='pending' WHERE id=? AND status='complete'",
                    [id],
                )? != 1
                {
                    return Err(FileError::RecoveryRequired(Box::new(
                        FileError::Unavailable,
                    )));
                }
                Ok(())
            })
            .map_err(FileError::from)?;
        let mut moved = false;
        let result = self.db.write_with(|tx| {
            let c = tx.connection();
            self.check_remote_trash_owner(c, subject, id)?;
            self.authorize_destination(c, subject, path, Capability::FilesCreate)?;
            self.before_remote_write(c, subject)?;
            if self.scan_trash_tree(c, subject, path, &copy, true)?.0 != entry.sha256 {
                return Err(FileError::Corrupt);
            }
            let target = self.parent(path)?.join(leaf(path));
            self.check_trash_directory(c, id, &copy)?;
            native::rename_absent(&copy, &target)?;
            moved = true;
            sync_move(&copy, &target)?;
            self.audit(c, subject, "file.restore", path, [("trash", Some(id.clone()))])?;
            c.execute("UPDATE file_trash SET status='restored' WHERE id=?", [id])?;
            let change = FileChange { sha256: entry.sha256.clone(), size: entry.size, trash: None };
            self.remote_complete(c, subject, json!({"outcome":"succeeded","sha256":change.sha256,"size":change.size.to_string(),"trash_id":null}))?;
            Ok(change)
        }).map_err(FileError::from);
        if result.is_err() && !moved {
            self.db.write(|c| {
                c.execute(
                    "UPDATE file_trash SET status='complete' WHERE id=? AND status='pending'",
                    [id],
                )?;
                Ok(())
            })?;
        }
        if moved {
            result.map_err(|e| FileError::RecoveryRequired(Box::new(e)))
        } else {
            result
        }
    }

    pub(super) fn reconcile_directory_trash(&self) -> Result<usize> {
        self.db.write_with(|tx| {
            let c = tx.connection();
            let mut query = c.prepare("SELECT id,path FROM file_trash WHERE server_id=? AND directory=1 AND status='pending' ORDER BY id")?;
            let rows = query.query_map([self.db.server_id()], |r| Ok((r.get::<_, String>(0)?,r.get::<_, String>(1)?)))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            for (id, path) in &rows {
                let location = self.directory_copy(id, false)?;
                let original = RelativePath::parse(path)?;
                let original = self.root.resolve(Some(&original)).is_ok_and(|path| {
                    path.metadata.is_dir() && self.check_trash_directory(c, id, &path.path).is_ok()
                });
                let status = match fs::symlink_metadata(&location) {
                    Ok(meta) if meta.is_dir() && self.check_trash_directory(c, id, &location).is_ok() => "complete",
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound && original => "restored",
                    _ => "recovery-required",
                };
                c.execute("UPDATE file_trash SET status=? WHERE id=?", params![status,id])?;
            }
            Ok(rows.len())
        }).map_err(FileError::from)
    }

    fn check_trash_directory(&self, c: &Connection, id: &str, path: &Path) -> Result<()> {
        let identity: String = c.query_row(
            "SELECT directory_identity FROM file_trash WHERE server_id=? AND id=?",
            params![self.db.server_id(), id],
            |row| row.get(0),
        )?;
        if native::directory_identity(path)? != identity {
            return Err(FileError::LinkedPath);
        }
        Ok(())
    }

    pub(in crate::files) fn remove_directory_copy(&self, c: &Connection, id: &str) -> Result<()> {
        let path = self.directory_copy(id, false)?;
        match fs::symlink_metadata(&path) {
            Ok(meta) => {
                super::super::root::plain(&meta)?;
                self.check_trash_directory(c, id, &path)?;
                // remove_dir_all removes interior links without following them.
                fs::remove_dir_all(path).map_err(io_error)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(io_error(e)),
        }
    }
}

fn sync_move(from: &Path, to: &Path) -> Result<()> {
    #[cfg(unix)]
    for path in [from, to] {
        let parent = path.parent().ok_or(FileError::InvalidPath)?;
        fs::File::open(parent)
            .and_then(|dir| dir.sync_all())
            .map_err(io_error)?;
    }
    #[cfg(not(unix))]
    let _ = (from, to);
    Ok(())
}
