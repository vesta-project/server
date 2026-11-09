use super::*;
use crate::content::{ContentEntry, ContentKind};
#[derive(Clone, Debug, Serialize)]
pub struct PackSuggestion {
    pub id: String,
    pub kind: String,
    pub entry_id: Option<String>,
    pub provider: Option<crate::content::ContentProvider>,
    pub path: Option<String>,
}
#[derive(Clone, Debug, Serialize)]
pub struct PackSuggestions {
    pub draft_revision: String,
    pub items: Vec<PackSuggestion>,
    pub warnings: Vec<String>,
    pub potentially_outdated: bool,
}
impl PackWorkspace {
    pub fn suggestions(
        &self,
        subject: &Subject,
        mc: &str,
        server: &[ContentEntry],
        truncated: bool,
        providers: &dyn WorkspaceProvider,
    ) -> Result<PackSuggestions> {
        let draft = self.read(subject, mc)?;
        let (mut items, mut warnings) =
            verified_server_suggestions(&draft, mc, server, truncated, providers);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
        for entry in &draft.manifest.entries {
            if std::time::Instant::now() > deadline {
                warnings.push("provider_budget_exhausted".into());
                break;
            }
            if let EntrySource::Provider {
                provider,
                project_id,
                version_id,
            } = &entry.source
            {
                match providers.newer(provider, project_id, version_id, &draft.manifest) {
                    Ok(Some(new)) => items.push(PackSuggestion {
                        id: format!("update:{}:{new}", entry.id),
                        kind: "newer_compatible_version".into(),
                        entry_id: Some(entry.id.clone()),
                        provider: Some(crate::content::ContentProvider {
                            provider: provider.clone(),
                            project_id: project_id.clone(),
                            version_id: new,
                        }),
                        path: Some(entry.path.clone()),
                    }),
                    Err(_) => warnings.push(format!("provider_unavailable:{}", entry.id)),
                    _ => {}
                }
            }
        }
        if truncated {
            warnings.push("server_inventory_truncated".into());
        }
        Ok(PackSuggestions {
            draft_revision: draft.revision,
            potentially_outdated: !items.is_empty(),
            items,
            warnings,
        })
    }
}
pub(super) fn verified_server_suggestions(
    draft: &PackDraft,
    mc: &str,
    server: &[ContentEntry],
    truncated: bool,
    providers: &dyn WorkspaceProvider,
) -> (Vec<PackSuggestion>, Vec<String>) {
    let mut server = server.to_vec();
    let mut warnings = Vec::new();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
    for item in &mut server {
        if item.kind != ContentKind::Mod || item.enabled != Some(true) {
            continue;
        }
        let Some(reference) = &item.provider else {
            continue;
        };
        if std::time::Instant::now() > deadline {
            warnings.push("provider_budget_exhausted".into());
            break;
        }
        match providers.pack_metadata(
            &reference.provider,
            &reference.project_id,
            &reference.version_id,
        ) {
            Ok(metadata) => {
                let declared = item.declared.get_or_insert_default();
                declared.side = metadata.client_required.then(|| "required".into());
            }
            Err(_) => {
                warnings.push(format!("provider_unavailable:{}", item.path));
                item.declared = None;
            }
        }
    }
    (server_suggestions(draft, mc, &server, truncated), warnings)
}
fn same_project(entry: &PackEntry, reference: &crate::content::ContentProvider) -> bool {
    matches!(&entry.source,EntrySource::Provider{provider,project_id,..} if provider==&reference.provider&&project_id==&reference.project_id)
}
pub(super) fn server_suggestions(
    draft: &PackDraft,
    mc: &str,
    server: &[ContentEntry],
    truncated: bool,
) -> Vec<PackSuggestion> {
    let mut items = Vec::new();
    if draft.manifest.minecraft_version != mc {
        items.push(PackSuggestion {
            id: format!("minecraft:{mc}"),
            kind: "minecraft_version_drift".into(),
            entry_id: None,
            provider: None,
            path: None,
        });
    }
    for item in server {
        if item.kind != ContentKind::Mod
            || item.enabled != Some(true)
            || matches!(item.pending.as_deref(), Some("remove" | "disable"))
        {
            continue;
        }
        let Some(reference) = &item.provider else {
            continue;
        };
        let needs_client = item
            .declared
            .as_ref()
            .and_then(|d| d.side.as_deref())
            .is_some_and(|s| s == "required");
        if needs_client
            && !draft
                .manifest
                .entries
                .iter()
                .any(|e| same_project(e, reference))
        {
            items.push(PackSuggestion {
                id: format!("missing:{}:{}", reference.provider, reference.project_id),
                kind: "client_mod_missing".into(),
                entry_id: None,
                provider: Some(reference.clone()),
                path: Some(item.path.clone()),
            });
        }
    }
    if !truncated {
        for entry in &draft.manifest.entries {
            if entry.kind != EntryKind::Mod || entry.environment.server != Side::Required {
                continue;
            }
            if let EntrySource::Provider {
                provider,
                project_id,
                version_id,
            } = &entry.source
            {
                let reference = crate::content::ContentProvider {
                    provider: provider.clone(),
                    project_id: project_id.clone(),
                    version_id: version_id.clone(),
                };
                if !server.iter().any(|s| {
                    s.kind == ContentKind::Mod
                        && s.enabled == Some(true)
                        && s.provider.as_ref().is_some_and(|r| {
                            r.provider == reference.provider && r.project_id == reference.project_id
                        })
                }) {
                    items.push(PackSuggestion {
                        id: format!("removed:{}", entry.id),
                        kind: "server_mod_removed".into(),
                        entry_id: Some(entry.id.clone()),
                        provider: Some(reference),
                        path: Some(entry.path.clone()),
                    });
                }
            }
        }
    }
    items
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn suggestions_are_read_only_distinguish_client_only_and_skip_truncated_removals() {
        let f = super::super::tests::Fixture::new();
        let mut manifest = f.manifest();
        manifest.entries.push(PackEntry {
            artifact_sha512: None,
            id: "both".into(),
            kind: EntryKind::Mod,
            path: "mods/both.jar".into(),
            required: true,
            environment: Environment {
                client: Side::Required,
                server: Side::Required,
            },
            source: EntrySource::Provider {
                provider: "modrinth".into(),
                project_id: "both".into(),
                version_id: "v1".into(),
            },
        });
        let draft = PackDraft {
            revision: "3".into(),
            manifest,
        };
        let server = ContentEntry {
            display_provider: None,
            modpack_owned: false,
            kind: ContentKind::Mod,
            path: "mods/needed.jar".into(),
            filename: "needed.jar".into(),
            size: "1".into(),
            enabled: Some(true),
            sha256: Some("a".repeat(64)),
            provider: Some(crate::content::ContentProvider {
                provider: "modrinth".into(),
                project_id: "needed".into(),
                version_id: "v2".into(),
            }),
            declared: Some(crate::content::DeclaredMetadata {
                side: Some("required".into()),
                ..Default::default()
            }),
            pending: None,
            pending_id: None,
            pending_error: None,
        };
        let suggestions =
            server_suggestions(&draft, "1.21.4", std::slice::from_ref(&server), false);
        assert_eq!(
            suggestions
                .iter()
                .map(|s| s.kind.as_str())
                .collect::<Vec<_>>(),
            vec![
                "minecraft_version_drift",
                "client_mod_missing",
                "server_mod_removed"
            ]
        );
        assert_eq!(
            server_suggestions(&draft, "1.21.1", &[server], true).len(),
            1
        );
        assert_eq!(
            f.workspace.read(&f.subject, "1.21.1").unwrap().revision,
            "0"
        );
    }
}
