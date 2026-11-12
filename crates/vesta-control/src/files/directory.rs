//! Journaled directory effects shared by remote file transports. Empty-only
//! removal and bounded rename; no recursive delete or replacement of a target.
use super::*;

impl FileBrowser {
    fn directory_authority(
        &self,
        c: &Connection,
        subject: &Subject,
        path: &RelativePath,
        cap: Capability,
    ) -> Result<super::super::root::CheckedPath> {
        self.authorize_destination(c, subject, path, cap)?;
        let checked = self.root.resolve(Some(path))?;
        if !checked.metadata.is_dir() {
            return Err(FileError::NotDirectory);
        }
        Ok(checked)
    }
    /// Require rename scope for every descendant at both old and prospective
    /// names. Linked/protected/ignored descendants refuse the entire operation.
    fn directory_rename_authority(
        &self,
        c: &Connection,
        subject: &Subject,
        from: &RelativePath,
        to: &RelativePath,
    ) -> Result<(PathBuf, PathBuf)> {
        let source = self.directory_authority(c, subject, from, Capability::FilesRename)?;
        self.authorize_destination(c, subject, to, Capability::FilesRename)?;
        let destination = self.parent(to)?.join(leaf(to));
        require_absent(&destination)?;
        if destination.starts_with(&source.path) {
            return Err(FileError::InvalidPath);
        }
        let policy = identity::policy_in(
            c,
            self.db.server_id(),
            self.db.clock().millis(),
            Some(subject),
        )?;
        let mut queue = vec![(from.clone(), to.clone())];
        let mut scanned = 0;
        while let Some((old, new)) = queue.pop() {
            let checked = self.root.resolve(Some(&old))?;
            let resource = self.root.resource(
                &self.parent(to)?.join(leaf(to)).join(
                    new.as_str()
                        .strip_prefix(to.as_str())
                        .unwrap_or("")
                        .trim_start_matches('/'),
                ),
            )?;
            super::super::authorize(&policy, Capability::FilesRename, &old.resource())?;
            super::super::authorize(&policy, Capability::FilesRename, &checked.resource)?;
            super::super::authorize(&policy, Capability::FilesRename, &new.resource())?;
            super::super::authorize(&policy, Capability::FilesRename, &resource)?;
            if checked.metadata.is_dir() {
                for entry in fs::read_dir(&checked.path).map_err(io_error)? {
                    scanned += 1;
                    if scanned > super::super::MAX_DIRECTORY_SCAN {
                        return Err(FileError::TooLarge);
                    }
                    let entry = entry.map_err(io_error)?;
                    let name = entry
                        .file_name()
                        .into_string()
                        .map_err(|_| FileError::InvalidPath)?;
                    queue.push((
                        RelativePath::parse(&format!("{}/{name}", old.as_str()))?,
                        RelativePath::parse(&format!("{}/{name}", new.as_str()))?,
                    ));
                }
            } else {
                let file = native::open_regular(&checked.path)?;
                let actual = native::actual_path(&file, &checked.path)?;
                super::super::authorize(
                    &policy,
                    Capability::FilesRename,
                    &self.root.resource(&actual)?,
                )?;
            }
        }
        Ok((source.path, destination))
    }
    pub fn rename_directory(
        &self,
        subject: &Subject,
        from: &RelativePath,
        to: &RelativePath,
    ) -> Result<()> {
        self.rename_directory_checked(subject, from, to, None)
    }
    pub(crate) fn rename_content_directory(
        &self,
        subject: &Subject,
        from: &RelativePath,
        to: &RelativePath,
        expected: &str,
    ) -> Result<()> {
        self.rename_directory_checked(subject, from, to, Some(expected))
    }
    fn rename_directory_checked(
        &self,
        subject: &Subject,
        from: &RelativePath,
        to: &RelativePath,
        expected: Option<&str>,
    ) -> Result<()> {
        let files = self.for_remote();
        let admission = mutation_admission(files.db.directory().path())?;
        let _guard = files.mutation_guard(&admission)?;
        if let Some(expected) = expected {
            let (hash, _) = files.content_tree(subject, from.as_str())?;
            if hash != expected {
                return Err(FileError::Conflict(Some(hash)));
            }
        }
        files
            .db
            .read_with(|tx| files.directory_rename_authority(tx.connection(), subject, from, to))
            .map_err(FileError::from)?;
        let intent = files.begin_intent(
            subject,
            "rename",
            from,
            &Intent {
                directory: true,
                to: Some(to.as_str().into()),
                ..Default::default()
            },
        )?;
        let mut applied = false;
        let result = files
            .db
            .write_with(|tx| {
                let c = tx.connection();
                let (source, destination) =
                    files.directory_rename_authority(c, subject, from, to)?;
                crash_point("before")?;
                native::rename_absent(&source, &destination)?;
                applied = true;
                crash_point("after")?;
                files.finish_intent(c, &intent)
            })
            .map_err(FileError::from);
        if interrupted() {
            return result;
        }
        let result = if applied {
            result.map_err(|e| FileError::RecoveryRequired(Box::new(e)))
        } else {
            result
        };
        files.failed_intent(&intent, &result)?;
        result
    }
    pub fn remove_empty_directory(&self, subject: &Subject, path: &RelativePath) -> Result<()> {
        let files = self.for_remote();
        let admission = mutation_admission(files.db.directory().path())?;
        let _guard = files.mutation_guard(&admission)?;
        files
            .db
            .read_with(|tx| {
                let checked = files.directory_authority(
                    tx.connection(),
                    subject,
                    path,
                    Capability::FilesDelete,
                )?;
                if fs::read_dir(&checked.path)
                    .map_err(io_error)?
                    .next()
                    .is_some()
                {
                    return Err(FileError::NotDirectory);
                }
                Ok(())
            })
            .map_err(FileError::from)?;
        let intent = files.begin_intent(
            subject,
            "delete",
            path,
            &Intent {
                directory: true,
                ..Default::default()
            },
        )?;
        let mut applied = false;
        let result = files
            .db
            .write_with(|tx| {
                let c = tx.connection();
                let checked =
                    files.directory_authority(c, subject, path, Capability::FilesDelete)?;
                if fs::read_dir(&checked.path)
                    .map_err(io_error)?
                    .next()
                    .is_some()
                {
                    return Err(FileError::NotDirectory);
                }
                crash_point("before")?;
                fs::remove_dir(&checked.path).map_err(io_error)?;
                applied = true;
                crash_point("after")?;
                files.finish_intent(c, &intent)
            })
            .map_err(FileError::from);
        if interrupted() {
            return result;
        }
        let result = if applied {
            result.map_err(|e| FileError::RecoveryRequired(Box::new(e)))
        } else {
            result
        };
        files.failed_intent(&intent, &result)?;
        result
    }
}
