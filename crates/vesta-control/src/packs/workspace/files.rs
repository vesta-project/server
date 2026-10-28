//! Virtual override tree. Blobs and trashed metadata stay in the bounded workspace;
//! no caller path is used as an operating-system path.
use super::*;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct OverrideFile {
    pub root: String,
    pub path: String,
    /// None is an explicit directory. Files reference immutable uploaded blobs.
    pub sha256: Option<String>,
    pub trashed: bool,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum FileOperation {
    Write {
        root: String,
        path: String,
        content_base64: String,
    },
    Attach {
        root: String,
        path: String,
        sha256: String,
    },
    Mkdir {
        root: String,
        path: String,
    },
    Rename {
        root: String,
        path: String,
        destination: String,
    },
    Trash {
        root: String,
        path: String,
    },
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FileMutation {
    pub expected_revision: String,
    pub change: FileOperation,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FileRead {
    pub root: String,
    pub path: String,
    pub expected_revision: String,
}
#[derive(Clone, Debug, Serialize)]
pub struct FileBody {
    pub revision: String,
    pub sha256: String,
    pub content_base64: String,
}

fn path(root: &str, name: &str) -> Result<()> {
    if !matches!(root, "overrides" | "client-overrides" | "server-overrides") {
        return Err(invalid());
    }
    vesta_protocol::pack::catalogue::RelativePath::parse(name)
        .map_err(super::super::PackError::from)?;
    // Apply precisely the existing game-file allowlist, including root option files.
    let entry = PackEntry {
        id: "validation".into(),
        kind: super::import::kind(name)?,
        path: name.into(),
        required: true,
        environment: Environment {
            client: Side::Required,
            server: Side::Optional,
        },
        source: EntrySource::Upload {
            sha256: "0".repeat(64),
        },
    };
    let manifest = PackManifest {
        name: "Validation".into(),
        summary: String::new(),
        icon: None,
        display_version: "1".into(),
        minecraft_version: "1".into(),
        loader: ClientLoader::None,
        loader_version: None,
        entries: vec![entry],
        override_files: Vec::new(),
    };
    validate_manifest(&manifest)
}
pub(super) fn validate_files(files: &[OverrideFile]) -> Result<()> {
    if files.len() > MAX_ENTRIES {
        return Err(super::super::PackError::TooLarge.into());
    }
    let mut names = BTreeSet::new();
    let mut components = std::collections::BTreeMap::new();
    for file in files {
        // Directory names must be safe but need not be a file in the game allowlist.
        vesta_protocol::pack::catalogue::RelativePath::parse(&file.path)
            .map_err(super::super::PackError::from)?;
        if !matches!(
            file.root.as_str(),
            "overrides" | "client-overrides" | "server-overrides"
        ) {
            return Err(invalid());
        }
        if let Some(sha) = &file.sha256 {
            path(&file.root, &file.path)?;
            if !valid_sha(sha) {
                return Err(invalid());
            }
        }
        if !file.trashed {
            let mut prefix = file.root.clone();
            for part in file.path.split('/') {
                prefix.push('/');
                prefix.push_str(part);
                if let Some(old) = components.insert(prefix.to_ascii_lowercase(), prefix.clone())
                    && old != prefix
                {
                    return Err(invalid());
                }
            }
        }
        if !file.trashed && !names.insert((file.root.clone(), file.path.to_ascii_lowercase())) {
            return Err(invalid());
        }
    }
    for a in files.iter().filter(|f| !f.trashed) {
        for b in files
            .iter()
            .filter(|f| !f.trashed && f.root == a.root && f.path != a.path)
        {
            if b.path
                .to_ascii_lowercase()
                .starts_with(&(a.path.to_ascii_lowercase() + "/"))
                && a.sha256.is_some()
            {
                return Err(invalid());
            }
        }
    }
    Ok(())
}
impl PackWorkspace {
    pub fn file_read(&self, subject: &Subject, mc: &str, input: &FileRead) -> Result<FileBody> {
        path(&input.root, &input.path)?;
        let draft = self.read(subject, mc)?;
        if draft.revision != input.expected_revision {
            return Err(ReleaseError::Conflict);
        }
        let sha = draft
            .manifest
            .override_files
            .iter()
            .find(|f| !f.trashed && f.root == input.root && f.path == input.path)
            .and_then(|f| f.sha256.clone())
            .ok_or_else(invalid)?;
        let bytes = self.db.read_with(|s| self.blob_in(s.connection(), &sha))?;
        // Inline text editor reads are bounded separately from binary attachments.
        if bytes.len() > 1024 * 1024 || std::str::from_utf8(&bytes).is_err() {
            return Err(super::super::PackError::TooLarge.into());
        }
        Ok(FileBody {
            revision: draft.revision,
            sha256: sha,
            content_base64: STANDARD.encode(bytes),
        })
    }
    pub fn file_mutate(
        &self,
        subject: &Subject,
        mc: &str,
        key: &str,
        input: &FileMutation,
    ) -> Result<PackDraft> {
        self.mutate(subject, key, input, |c| {
            let mut manifest = self.read_in(c, mc)?.manifest;
            let files = &mut manifest.override_files;
            let (root, name) = match &input.change {
                FileOperation::Write { root, path, .. }
                | FileOperation::Attach { root, path, .. }
                | FileOperation::Mkdir { root, path }
                | FileOperation::Rename { root, path, .. }
                | FileOperation::Trash { root, path } => (root, path),
            };
            vesta_protocol::pack::catalogue::RelativePath::parse(name)
                .map_err(super::super::PackError::from)?;
            if !matches!(
                root.as_str(),
                "overrides" | "client-overrides" | "server-overrides"
            ) {
                return Err(invalid());
            }
            match &input.change {
                FileOperation::Write { content_base64, .. }
                | FileOperation::Attach {
                    sha256: content_base64,
                    ..
                } => {
                    path(root, name)?;
                    let sha = if matches!(input.change, FileOperation::Write { .. }) {
                        let bytes = STANDARD.decode(content_base64).map_err(|_| invalid())?;
                        if bytes.len() > 1024 * 1024 || std::str::from_utf8(&bytes).is_err() {
                            return Err(super::super::PackError::TooLarge.into());
                        }
                        self.store_blob(c, &bytes)?
                    } else {
                        self.blob_in(c, content_base64)?;
                        content_base64.clone()
                    };
                    if let Some(file) = files
                        .iter_mut()
                        .find(|f| !f.trashed && f.root == *root && f.path == *name)
                    {
                        if file.sha256.is_none() {
                            return Err(invalid());
                        }
                        file.sha256 = Some(sha);
                    } else {
                        files.push(OverrideFile {
                            root: root.clone(),
                            path: name.clone(),
                            sha256: Some(sha),
                            trashed: false,
                        });
                    }
                }
                FileOperation::Mkdir { .. } => files.push(OverrideFile {
                    root: root.clone(),
                    path: name.clone(),
                    sha256: None,
                    trashed: false,
                }),
                FileOperation::Rename { destination, .. } => {
                    if destination.starts_with(&(name.clone() + "/")) {
                        return Err(invalid());
                    }
                    let mut found = false;
                    for file in files.iter_mut().filter(|f| !f.trashed && f.root == *root) {
                        if file.path == *name || file.path.starts_with(&(name.clone() + "/")) {
                            file.path = format!("{destination}{}", &file.path[name.len()..]);
                            found = true;
                        }
                    }
                    if !found {
                        return Err(invalid());
                    }
                }
                FileOperation::Trash { .. } => {
                    let mut found = false;
                    for file in files.iter_mut().filter(|f| !f.trashed && f.root == *root) {
                        if file.path == *name || file.path.starts_with(&(name.clone() + "/")) {
                            file.trashed = true;
                            found = true;
                        }
                    }
                    if !found {
                        return Err(invalid());
                    }
                }
            }
            self.write_in(c, subject, mc, &input.expected_revision, manifest)
        })
    }
    /// Shared first, then client-specific files. Server files never enter client artifacts.
    pub(super) fn client_manifest(&self, manifest: &PackManifest) -> Result<PackManifest> {
        validate_files(&manifest.override_files)?;
        let mut result = manifest.clone();
        for root in ["overrides", "client-overrides"] {
            for file in manifest
                .override_files
                .iter()
                .filter(|f| !f.trashed && f.root == root)
            {
                let Some(sha) = &file.sha256 else {
                    continue;
                };
                result
                    .entries
                    .retain(|e| !e.path.eq_ignore_ascii_case(&file.path));
                result.entries.push(PackEntry {
                    id: format!("override-{}", result.entries.len()),
                    kind: super::import::kind(&file.path)?,
                    path: file.path.clone(),
                    required: true,
                    environment: Environment {
                        client: Side::Required,
                        server: if root == "overrides" {
                            Side::Optional
                        } else {
                            Side::Unsupported
                        },
                    },
                    source: EntrySource::Upload {
                        sha256: sha.clone(),
                    },
                });
            }
        }
        result.override_files.clear();
        validate_manifest(&result)?;
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::packs::workspace::tests::Fixture;
    #[test]
    fn override_tree_is_revisioned_and_client_precedence_excludes_server_files() {
        let f = Fixture::new();
        for (i, (root, body)) in [
            ("overrides", "shared"),
            ("client-overrides", "client"),
            ("server-overrides", "server"),
        ]
        .iter()
        .enumerate()
        {
            f.workspace
                .file_mutate(
                    &f.subject,
                    "1.21.1",
                    &format!("write-{i}"),
                    &FileMutation {
                        expected_revision: i.to_string(),
                        change: FileOperation::Write {
                            root: (*root).into(),
                            path: "config/a.txt".into(),
                            content_base64: STANDARD.encode(body),
                        },
                    },
                )
                .unwrap();
        }
        let draft = f.workspace.read(&f.subject, "1.21.1").unwrap();
        let client = f.workspace.client_manifest(&draft.manifest).unwrap();
        assert_eq!(client.entries.len(), 1);
        assert_eq!(
            client.entries[0].source,
            EntrySource::Upload {
                sha256: digest(b"client")
            }
        );
        let input = FileMutation {
            expected_revision: "3".into(),
            change: FileOperation::Trash {
                root: "client-overrides".into(),
                path: "config".into(),
            },
        };
        let trash = f
            .workspace
            .file_mutate(&f.subject, "1.21.1", "trash", &input)
            .unwrap();
        assert_eq!(trash.revision, "4");
        assert!(trash.manifest.override_files[1].trashed);
        assert_eq!(
            f.workspace
                .file_mutate(&f.subject, "1.21.1", "trash", &input)
                .unwrap()
                .revision,
            "4"
        );
        assert!(
            f.workspace
                .file_mutate(&f.subject, "1.21.1", "stale", &input)
                .is_err()
        );
        let client = f.workspace.client_manifest(&trash.manifest).unwrap();
        assert_eq!(
            client.entries[0].source,
            EntrySource::Upload {
                sha256: digest(b"shared")
            }
        );
        assert!(
            f.workspace
                .file_read(
                    &f.subject,
                    "1.21.1",
                    &FileRead {
                        root: "client-overrides".into(),
                        path: "config/a.txt".into(),
                        expected_revision: "4".into()
                    }
                )
                .is_err()
        );
    }
    #[test]
    fn invalid_names_aliases_and_file_directory_collisions_refuse() {
        assert!(path("other", "config/a").is_err());
        assert!(path("overrides", "../secret").is_err());
        assert!(path("overrides", "server.properties").is_err());
        let file = OverrideFile {
            root: "overrides".into(),
            path: "config/a".into(),
            sha256: Some("a".repeat(64)),
            trashed: false,
        };
        let mut other = file.clone();
        other.path = "config/A".into();
        assert!(validate_files(&[file.clone(), other.clone()]).is_err());
        other.path = "config/a/nested".into();
        assert!(validate_files(&[file, other]).is_err());
    }
}
