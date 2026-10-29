//! Server installation identity is independent from the client workspace.
use super::*;
use std::collections::BTreeMap;
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ServerModpackLink {
    pub source: InstallSource,
    pub display_version: String,
    pub files: BTreeMap<String, String>,
    pub state: String,
    #[serde(default)]
    pub pending_ids: Vec<String>,
    /// Confirmed version; never advances while content is pending or cancelled.
    #[serde(default)]
    pub installed: Option<ServerModpackVersion>,
    /// Reviewed incoming version, promoted only after all journal entries apply.
    #[serde(default)]
    pub pending: Option<ServerModpackVersion>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ServerModpackVersion {
    pub source: InstallSource,
    pub display_version: String,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ClientPackMode {
    #[default]
    None,
    ServerModpack,
    Custom,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClientPackConfig {
    pub mode: ClientPackMode,
    pub require_update: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerModpackUnlink {
    pub remove_owned_files: bool,
}
pub(super) fn initialize(db: &StateDatabase) -> Result<()> {
    db.write(|c| c.execute_batch("CREATE TABLE IF NOT EXISTS server_modpack_link(server_id TEXT PRIMARY KEY, body TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS server_modpack_files(path TEXT PRIMARY KEY, sha256 TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS client_pack_config(server_id TEXT PRIMARY KEY, body TEXT NOT NULL);").map_err(Into::into))?;
    db.write(|c| {
        let published: bool = c.query_row(
            "SELECT EXISTS(SELECT 1 FROM pack_releases WHERE server_id=?)",
            [db.server_id()],
            |r| r.get(0),
        )?;
        let config = ClientPackConfig {
            mode: if published {
                ClientPackMode::Custom
            } else {
                ClientPackMode::None
            },
            require_update: false,
        };
        c.execute(
            "INSERT OR IGNORE INTO client_pack_config VALUES(?,?)",
            params![
                db.server_id(),
                serde_json::to_string(&config)
                    .map_err(|_| vesta_state::StateError::new("client_pack_config_invalid"))?
            ],
        )?;
        Ok(())
    })?;
    Ok(())
}
impl PackWorkspace {
    pub(super) fn server_link_in(&self, c: &Connection) -> Result<Option<ServerModpackLink>> {
        let raw: Option<String> = c
            .query_row(
                "SELECT body FROM server_modpack_link WHERE server_id=?",
                [self.db.server_id()],
                |r| r.get(0),
            )
            .optional()?;
        let mut link: Option<ServerModpackLink> = raw
            .map(|raw| serde_json::from_str(&raw).map_err(|_| invalid()))
            .transpose()?;
        if let Some(link) = &mut link {
            let mut pending = false;
            for id in &link.pending_ids {
                let state: Option<String> = c
                    .query_row(
                        "SELECT state FROM server_content_changes WHERE id=?",
                        [id],
                        |r| r.get(0),
                    )
                    .optional()?;
                if state.as_deref() != Some("applied") {
                    pending = true;
                }
            }
            let selected = ServerModpackVersion {
                source: link.source.clone(),
                display_version: link.display_version.clone(),
            };
            if pending {
                link.state = "staged_until_restart".into();
                link.pending = Some(selected);
            } else {
                link.state = "installed".into();
                link.installed = Some(selected);
                link.pending = None;
            }
        }
        Ok(link)
    }
    pub fn server_link(&self, s: &Subject) -> Result<Option<ServerModpackLink>> {
        self.db
            .read_with(|t| {
                self.authorize_in(t.connection(), s, Capability::PackEdit)?;
                self.server_link_in(t.connection())
            })
            .map_err(Into::into)
    }
    /// Legacy provider pointers have no installation receipt; retain as custom.
    pub fn client_pack(&self, s: &Subject, has_legacy_release: bool) -> Result<ClientPackConfig> {
        self.db
            .read_with(|t| {
                let c = t.connection();
                self.authorize_in(c, s, Capability::PackEdit)?;
                let raw: Option<String> = c
                    .query_row(
                        "SELECT body FROM client_pack_config WHERE server_id=?",
                        [self.db.server_id()],
                        |r| r.get(0),
                    )
                    .optional()?;
                match raw {
                    Some(raw) => serde_json::from_str(&raw).map_err(|_| invalid()),
                    None => Ok(ClientPackConfig {
                        mode: if has_legacy_release {
                            ClientPackMode::Custom
                        } else {
                            ClientPackMode::None
                        },
                        require_update: false,
                    }),
                }
            })
            .map_err(Into::into)
    }
    pub fn set_client_pack(
        &self,
        s: &Subject,
        key: &str,
        input: &ClientPackConfig,
    ) -> Result<ClientPackConfig> {
        self.mutate(s, key, input, |c| {
            self.authorize_in(c, s, Capability::PackPublish)?;
            if input.require_update {
                self.authorize_in(c, s, Capability::PackForceUpdate)?;
            }
            if input.mode == ClientPackMode::ServerModpack && self.server_link_in(c)?.is_none() {
                return Err(ReleaseError::Conflict);
            }
            c.execute(
                "INSERT OR REPLACE INTO client_pack_config VALUES(?,?)",
                params![
                    self.db.server_id(),
                    serde_json::to_string(input).map_err(|_| invalid())?
                ],
            )?;
            Ok(input.clone())
        })
    }
    pub fn unlink_server_modpack(
        &self,
        s: &Subject,
        key: &str,
        input: &ServerModpackUnlink,
        files: &crate::files::FileBrowser,
    ) -> Result<PackInstallOutcome> {
        let _guard = files.pack_admission().map_err(|_| ReleaseError::Conflict)?;
        self.mutate(s, key, input, |c| {
            self.install_authority(c, s)?;
            let link = self.server_link_in(c)?.ok_or(ReleaseError::Conflict)?;
            // Do not orphan staged writes or discard their installation receipt.
            if link.pending.is_some() {
                return Err(ReleaseError::Conflict);
            }
            let removals = if input.remove_owned_files {
                link.files.into_iter().collect::<Vec<_>>()
            } else {
                Vec::new()
            };
            let pending_ids = files
                .stage_pack_removals_in(c, s, key, &removals)
                .map_err(|_| ReleaseError::Conflict)?;
            c.execute(
                "DELETE FROM server_modpack_link WHERE server_id=?",
                [self.db.server_id()],
            )?;
            c.execute("DELETE FROM server_modpack_files", [])?;
            let raw: Option<String> = c
                .query_row(
                    "SELECT body FROM client_pack_config WHERE server_id=?",
                    [self.db.server_id()],
                    |r| r.get(0),
                )
                .optional()?;
            if let Some(raw) = raw {
                let config: ClientPackConfig = serde_json::from_str(&raw).map_err(|_| invalid())?;
                if config.mode == ClientPackMode::ServerModpack {
                    c.execute(
                        "UPDATE client_pack_config SET body=? WHERE server_id=?",
                        params![
                            serde_json::to_string(&ClientPackConfig::default())
                                .map_err(|_| invalid())?,
                            self.db.server_id()
                        ],
                    )?;
                }
            }
            Ok(PackInstallOutcome {
                state: "staged_until_restart".into(),
                pending_ids,
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::packs::workspace::tests::Fixture;
    #[test]
    fn client_modes_are_independent_and_legacy_pointers_do_not_infer_installation() {
        let f = Fixture::new();
        assert!(f.workspace.server_link(&f.subject).unwrap().is_none());
        assert_eq!(
            f.workspace.client_pack(&f.subject, false).unwrap().mode,
            ClientPackMode::None
        );
        assert_eq!(
            f.workspace.client_pack(&f.subject, true).unwrap().mode,
            ClientPackMode::None
        );
        let follow = ClientPackConfig {
            mode: ClientPackMode::ServerModpack,
            require_update: true,
        };
        assert!(
            f.workspace
                .set_client_pack(&f.subject, "missing-link", &follow)
                .is_err()
        );
        let link = ServerModpackLink {
            source: InstallSource::Provider {
                provider: "modrinth".into(),
                project_id: "pack".into(),
                version_id: "v1".into(),
            },
            display_version: "1".into(),
            files: BTreeMap::new(),
            state: "installed".into(),
            pending_ids: Vec::new(),
            installed: None,
            pending: None,
        };
        f.db.write(|c| {
            c.execute(
                "INSERT INTO server_modpack_link VALUES(?,?)",
                params![f.db.server_id(), serde_json::to_string(&link).unwrap()],
            )
            .map(|_| ())
            .map_err(Into::into)
        })
        .unwrap();
        f.workspace
            .set_client_pack(&f.subject, "follow", &follow)
            .unwrap();
        assert_eq!(
            f.workspace.client_pack(&f.subject, false).unwrap().mode,
            ClientPackMode::ServerModpack
        );
        f.workspace
            .set_client_pack(
                &f.subject,
                "custom",
                &ClientPackConfig {
                    mode: ClientPackMode::Custom,
                    require_update: false,
                },
            )
            .unwrap();
        assert!(f.workspace.server_link(&f.subject).unwrap().is_some());
        assert_eq!(
            f.workspace.client_pack(&f.subject, false).unwrap().mode,
            ClientPackMode::Custom
        );
    }
}

#[cfg(test)]
mod migration_tests {
    use super::*;
    use crate::packs::{PackReleases, workspace::tests::Fixture};
    #[test]
    fn legacy_provider_publication_migrates_as_custom_without_server_ownership() {
        let f = Fixture::new();
        let releases = PackReleases::open(&f.db).unwrap();
        f.workspace
            .publish_provider_pack(
                &f.subject,
                "1.21.1",
                "legacy-provider",
                &ProviderPackPublish {
                    expected_revision: "0".into(),
                    draft_revision: "0".into(),
                    provider: "modrinth".into(),
                    project_id: "pack".into(),
                    version_id: "v1".into(),
                    required_update: false,
                    acknowledge_lower_version: false,
                },
                &releases,
            )
            .unwrap();
        f.db.write(|c| {
            c.execute("DELETE FROM client_pack_config", [])
                .map(|_| ())
                .map_err(Into::into)
        })
        .unwrap();
        let migrated = PackWorkspace::open(&f.db).unwrap();
        assert_eq!(
            migrated.client_pack(&f.subject, true).unwrap().mode,
            ClientPackMode::Custom
        );
        assert!(migrated.server_link(&f.subject).unwrap().is_none());
        let count: i64 =
            f.db.read(|c| {
                c.query_row("SELECT count(*) FROM server_modpack_files", [], |r| {
                    r.get(0)
                })
                .map_err(Into::into)
            })
            .unwrap();
        assert_eq!(count, 0);
    }
}
