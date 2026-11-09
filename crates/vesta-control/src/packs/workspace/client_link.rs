//! Provider ownership lives in the revisioned draft and is published with it.
use super::*;
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ClientModpackLink {
    pub provider: String,
    pub project_id: String,
    pub version_id: String,
    pub entries: Vec<String>,
    pub overrides: Vec<(String, String)>,
}
pub(super) fn unlink(manifest: &mut PackManifest, choice: &str) -> Result<()> {
    if !matches!(
        choice,
        "keep_everything" | "keep_resources" | "remove_everything"
    ) {
        return Err(invalid());
    }
    if let Some(link) = manifest.modpack_link.take() {
        if choice != "keep_everything" {
            manifest
                .override_files
                .retain(|f| !link.overrides.contains(&(f.root.clone(), f.path.clone())));
            manifest.entries.retain(|e| {
                !link.entries.contains(&e.id)
                    || (choice == "keep_resources" && e.kind != EntryKind::ConfigOverride)
            });
        }
    }
    Ok(())
}
pub(super) fn link(
    custom: &mut PackManifest,
    mut base: PackManifest,
    source: crate::content::ContentProvider,
    mc: &str,
) -> Result<()> {
    if base.minecraft_version != mc {
        return Err(invalid());
    }
    unlink(custom, "remove_everything")?;
    // Reject collisions rather than silently taking ownership of user additions.
    if base.entries.iter().any(|e| {
        custom
            .entries
            .iter()
            .any(|c| c.id == e.id || c.path == e.path)
    }) || base.override_files.iter().any(|f| {
        custom
            .override_files
            .iter()
            .any(|c| c.root == f.root && c.path == f.path)
    }) {
        return Err(ReleaseError::Conflict);
    }
    custom.modpack_link = Some(ClientModpackLink {
        provider: source.provider,
        project_id: source.project_id,
        version_id: source.version_id,
        entries: base.entries.iter().map(|e| e.id.clone()).collect(),
        overrides: base
            .override_files
            .iter()
            .map(|f| (f.root.clone(), f.path.clone()))
            .collect(),
    });
    custom.loader = base.loader;
    custom.loader_version = base.loader_version;
    custom.entries.append(&mut base.entries);
    custom.override_files.append(&mut base.override_files);
    Ok(())
}

// Ordinary draft edits cannot forge ownership or modify provider-owned content.
pub(super) fn check_edit(old: &PackManifest, next: &PackManifest) -> Result<()> {
    if old.modpack_link != next.modpack_link {
        return Err(ReleaseError::Conflict);
    }
    if let Some(link) = &old.modpack_link {
        if old.loader != next.loader || old.loader_version != next.loader_version {
            return Err(ReleaseError::Conflict);
        }
        for id in &link.entries {
            if old.entries.iter().find(|e| &e.id == id) != next.entries.iter().find(|e| &e.id == id)
            {
                return Err(ReleaseError::Conflict);
            }
        }
        for (root, path) in &link.overrides {
            if old
                .override_files
                .iter()
                .find(|f| &f.root == root && &f.path == path)
                != next
                    .override_files
                    .iter()
                    .find(|f| &f.root == root && &f.path == path)
            {
                return Err(ReleaseError::Conflict);
            }
        }
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::super::tests::Fixture;
    use super::*;
    fn source(version: &str) -> crate::content::ContentProvider {
        crate::content::ContentProvider {
            provider: "modrinth".into(),
            project_id: "pack".into(),
            version_id: version.into(),
        }
    }
    fn base(f: &Fixture, id: &str) -> PackManifest {
        let mut m = f.manifest();
        m.entries.push(PackEntry {
            artifact_sha512: None,
            id: id.into(),
            path: format!("mods/{id}.jar"),
            kind: EntryKind::Mod,
            required: true,
            environment: Environment {
                client: Side::Required,
                server: Side::Optional,
            },
            source: EntrySource::Provider {
                provider: "modrinth".into(),
                project_id: id.into(),
                version_id: "1".into(),
            },
        });
        m.override_files.push(OverrideFile {
            root: "overrides".into(),
            path: "config/pack.txt".into(),
            sha256: Some("a".repeat(64)),
            trashed: false,
        });
        m
    }
    #[test]
    fn switch_preserves_additions_and_replaces_owned_files() {
        let f = Fixture::new();
        let mut m = f.manifest();
        m.entries = base(&f, "custom").entries;
        link(&mut m, base(&f, "v1"), source("v1"), "1.21.1").unwrap();
        let previous = m.clone();
        link(&mut m, base(&f, "v2"), source("v2"), "1.21.1").unwrap();
        assert_eq!(
            m.entries.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(),
            ["custom", "v2"]
        );
        assert!(check_edit(&previous, &m).is_err());
        let mut edited = m.clone();
        edited.entries[1].required = false;
        assert!(check_edit(&m, &edited).is_err());
        edited = m.clone();
        edited.entries[0].required = false;
        check_edit(&m, &edited).unwrap();
    }
    #[test]
    fn all_unlink_choices_keep_user_additions() {
        let f = Fixture::new();
        for (choice, entries, overrides) in [
            ("keep_everything", 2, 1),
            ("keep_resources", 2, 0),
            ("remove_everything", 1, 0),
        ] {
            let mut m = f.manifest();
            m.entries = base(&f, "custom").entries;
            link(&mut m, base(&f, "pack"), source("1"), "1.21.1").unwrap();
            unlink(&mut m, choice).unwrap();
            assert!(m.modpack_link.is_none());
            assert_eq!(m.entries.len(), entries);
            assert_eq!(m.override_files.len(), overrides);
            assert_eq!(m.entries[0].id, "custom");
        }
    }
    #[test]
    fn unlink_is_durable_revision_guarded_and_replays_without_publication() {
        let f = Fixture::new();
        let mut m = f.manifest();
        link(&mut m, base(&f, "pack"), source("1"), "1.21.1").unwrap();
        // Use a directory override here: the blob store is tested by import/file tests.
        m.override_files[0].sha256 = None;
        let draft = f.workspace.mutate(&f.subject, "seed-link", &"seed", |c| f.workspace.write_link_in(c, &f.subject, "1.21.1", "0", m)).unwrap();
        let releases = crate::packs::PackReleases::open(&f.db).unwrap();
        let request = ImportRequest::Unlink { expected_revision: draft.revision.clone(), choice: "keep_resources".into() };
        let providers = crate::content::UreqProviderClient::new(None);
        let first = f.workspace.import(&f.subject, "1.21.1", "unlink", &request, &releases, &providers).unwrap();
        assert_eq!(first, f.workspace.import(&f.subject, "1.21.1", "unlink", &request, &releases, &providers).unwrap());
        assert!(f.workspace.import(&f.subject, "1.21.1", "stale", &request, &releases, &providers).is_err());
        assert!(releases.current().unwrap().is_none());
        assert!(PackWorkspace::open(&f.db).unwrap().read(&f.subject, "1.21.1").unwrap().manifest.modpack_link.is_none());
    }
    #[test]
    fn collisions_and_runtime_mismatch_are_rejected() {
        let f = Fixture::new();
        let mut m = base(&f, "same");
        assert!(link(&mut m, base(&f, "same"), source("1"), "1.21.1").is_err());
        assert!(link(&mut f.manifest(), base(&f, "other"), source("1"), "1.20.1").is_err());
    }
}
