//! Bounded archive ingestion. Imported download URLs never become fetch authority.
use super::*;
use crate::content::{Provider, ProviderClient, UreqProviderClient};
use std::io::{Cursor, Read};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "source", rename_all = "snake_case", deny_unknown_fields)]
pub enum ImportRequest {
    Archive {
        expected_revision: String,
        content_base64: String,
    },
    Published {
        expected_revision: String,
        expected_published_revision: String,
    },
}
impl PackWorkspace {
    pub fn import(
        &self,
        subject: &Subject,
        mc: &str,
        key: &str,
        input: &ImportRequest,
        releases: &super::super::PackReleases,
        providers: &UreqProviderClient,
    ) -> Result<PackDraft> {
        if let Some(outcome) = self.replay(subject, key, input)? {
            return Ok(outcome);
        }
        let expected = match input {
            ImportRequest::Archive {
                expected_revision, ..
            }
            | ImportRequest::Published {
                expected_revision, ..
            } => expected_revision,
        };
        if self.read(subject, mc)?.revision != *expected {
            return Err(ReleaseError::Conflict);
        }
        let mut release_revision = None;
        let (manifest, blobs) = match input {
            ImportRequest::Archive { content_base64, .. } => parse_archive(
                &STANDARD.decode(content_base64).map_err(|_| invalid())?,
                providers,
                |hash| {
                    providers
                        .identify_sha512(hash)
                        .map_err(|_| super::super::PackError::UnresolvedFile.into())
                },
            )?,
            ImportRequest::Published {
                expected_published_revision,
                ..
            } => {
                let current = releases.current()?.ok_or(ReleaseError::Conflict)?;
                if current.revision() != revision(expected_published_revision)? {
                    return Err(ReleaseError::Conflict);
                }
                release_revision = Some(current.revision());
                if let Some((r, manifest)) = self.published(subject)?
                    && r == current.revision()
                {
                    (manifest, Vec::new())
                } else {
                    let loaded = releases
                        .artifacts()
                        .load(current.artifact(), super::super::MAX_ARTIFACT_BYTES)?;
                    parse_archive(loaded.bytes(), providers, |hash| {
                        providers
                            .identify_sha512(hash)
                            .map_err(|_| super::super::PackError::UnresolvedFile.into())
                    })?
                }
            }
        };
        self.mutate(subject, key, input, |c| {
            if let Some(release_revision) = release_revision {
                let actual: i64 = c.query_row(
                    "SELECT coalesce(max(revision),0) FROM pack_releases WHERE server_id=?",
                    [self.db.server_id()],
                    |r| r.get(0),
                )?;
                if actual as u64 != release_revision {
                    return Err(ReleaseError::Conflict);
                }
            }
            for bytes in blobs {
                self.store_blob(c, &bytes)?;
            }
            self.write_in(c, subject, mc, expected, manifest)
        })
    }
}
fn string<'a>(v: &'a serde_json::Value, key: &str) -> Result<&'a str> {
    v.get(key).and_then(|v| v.as_str()).ok_or_else(invalid)
}
fn kind(path: &str) -> Result<EntryKind> {
    match path.split('/').next().unwrap_or_default() {
        "mods" => Ok(EntryKind::Mod),
        "resourcepacks" => Ok(EntryKind::ResourcePack),
        "shaderpacks" => Ok(EntryKind::Shader),
        "datapacks" => Ok(EntryKind::Datapack),
        "config" => Ok(EntryKind::ConfigOverride),
        _ => Err(invalid()),
    }
}
fn entry(
    id: String,
    path: String,
    source: EntrySource,
    required: bool,
    server: Side,
) -> Result<PackEntry> {
    Ok(PackEntry {
        id,
        kind: kind(&path)?,
        path,
        required,
        environment: Environment {
            client: if required {
                Side::Required
            } else {
                Side::Optional
            },
            server,
        },
        source,
    })
}
fn parse_archive(
    bytes: &[u8],
    providers: &dyn ProviderClient,
    identify: impl Fn(&str) -> Result<Option<crate::content::ContentProvider>>,
) -> Result<(PackManifest, Vec<Vec<u8>>)> {
    if bytes.len() > super::super::MAX_ARTIFACT_BYTES {
        return Err(super::super::PackError::TooLarge.into());
    }
    let mut zip = zip::ZipArchive::new(Cursor::new(bytes)).map_err(|_| invalid())?;
    if zip.len() > super::super::MAX_VESTA_ENTRIES {
        return Err(super::super::PackError::TooLarge.into());
    }
    let mut files = std::collections::BTreeMap::new();
    let mut total = 0usize;
    for i in 0..zip.len() {
        let f = zip.by_index(i).map_err(|_| invalid())?;
        let name = f.name().to_string();
        vesta_protocol::pack::catalogue::RelativePath::parse(name.trim_end_matches('/'))
            .map_err(super::super::PackError::from)?;
        if f.is_symlink() || f.encrypted() || f.size() > super::super::MAX_PINNED_FILE_BYTES as u64
        {
            return Err(invalid());
        }
        if f.is_dir() {
            continue;
        }
        total = total
            .checked_add(f.size() as usize)
            .filter(|n| *n <= MAX_BLOBS)
            .ok_or(super::super::PackError::TooLarge)?;
        let mut body = Vec::new();
        f.take(super::super::MAX_PINNED_FILE_BYTES as u64 + 1)
            .read_to_end(&mut body)
            .map_err(|_| invalid())?;
        if body.len() > super::super::MAX_PINNED_FILE_BYTES || files.insert(name, body).is_some() {
            return Err(invalid());
        }
    }
    let mr = files.remove("modrinth.index.json");
    let cf = files.remove("manifest.json");
    if mr.is_some() == cf.is_some() {
        return Err(invalid());
    }
    let document: serde_json::Value =
        serde_json::from_slice(mr.as_ref().or(cf.as_ref()).ok_or_else(invalid)?)
            .map_err(|_| invalid())?;
    let mut manifest = PackManifest {
        name: string(&document, "name")?.into(),
        summary: document["summary"].as_str().unwrap_or_default().into(),
        icon: None,
        display_version: document["versionId"]
            .as_str()
            .or(document["version"].as_str())
            .filter(|s| {
                s.split('.')
                    .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
            })
            .unwrap_or("1.0")
            .into(),
        minecraft_version: String::new(),
        loader: ClientLoader::None,
        loader_version: None,
        entries: Vec::new(),
    };
    if mr.is_some() {
        if document["formatVersion"] != 1 || document["game"] != "minecraft" {
            return Err(invalid());
        }
        manifest.minecraft_version = string(&document["dependencies"], "minecraft")?.into();
        for (key, loader) in [
            ("fabric-loader", ClientLoader::Fabric),
            ("quilt-loader", ClientLoader::Quilt),
            ("forge", ClientLoader::Forge),
            ("neoforge", ClientLoader::Neoforge),
        ] {
            if let Some(version) = document["dependencies"][key].as_str() {
                if manifest.loader != ClientLoader::None {
                    return Err(invalid());
                }
                manifest.loader = loader;
                manifest.loader_version = Some(version.into());
            }
        }
        let rows = document["files"].as_array().ok_or_else(invalid)?;
        if rows.len() > MAX_ENTRIES {
            return Err(invalid());
        }
        for (i, row) in rows.iter().enumerate() {
            if row["env"]["client"] == "unsupported" {
                continue;
            }
            let reference = if let Some(hash) = row["hashes"]["sha512"].as_str() {
                identify(hash)?
            } else {
                None
            };
            let source = reference.map_or(
                EntrySource::Manual {
                    reason: "import_provider_identity_missing".into(),
                },
                |r| EntrySource::Provider {
                    provider: r.provider,
                    project_id: r.project_id,
                    version_id: r.version_id,
                },
            );
            let server = match row["env"]["server"].as_str() {
                Some("required") => Side::Required,
                Some("optional") => Side::Optional,
                _ => Side::Unsupported,
            };
            manifest.entries.push(entry(
                format!("import-{i}"),
                string(row, "path")?.into(),
                source,
                row["env"]["client"] != "optional",
                server,
            )?);
        }
    } else {
        if document["manifestType"] != "minecraftModpack" || document["manifestVersion"] != 1 {
            return Err(invalid());
        }
        manifest.minecraft_version = string(&document["minecraft"], "version")?.into();
        let loaders = document["minecraft"]["modLoaders"]
            .as_array()
            .ok_or_else(invalid)?;
        for row in loaders {
            if row["primary"] == false {
                continue;
            }
            let id = string(row, "id")?;
            let (prefix, version) = id.split_once('-').ok_or_else(invalid)?;
            if manifest.loader != ClientLoader::None {
                return Err(invalid());
            }
            manifest.loader = match prefix {
                "fabric" => ClientLoader::Fabric,
                "quilt" => ClientLoader::Quilt,
                "forge" => ClientLoader::Forge,
                "neoforge" => ClientLoader::Neoforge,
                _ => return Err(invalid()),
            };
            manifest.loader_version = Some(version.into());
        }
        let rows = document["files"].as_array().ok_or_else(invalid)?;
        if rows.len() > MAX_ENTRIES {
            return Err(invalid());
        }
        for (i, row) in rows.iter().enumerate() {
            let project = row["projectID"].as_u64().ok_or_else(invalid)?.to_string();
            let version = row["fileID"].as_u64().ok_or_else(invalid)?.to_string();
            let source = providers.resolve(Provider::Curseforge, &project, &version);
            let path = source.as_ref().map_or_else(
                |_| format!("mods/manual-{project}-{version}.jar"),
                |s| format!("mods/{}", s.filename),
            );
            manifest.entries.push(entry(
                format!("import-{i}"),
                path,
                EntrySource::Provider {
                    provider: "curseforge".into(),
                    project_id: project,
                    version_id: version,
                },
                row["required"] != false,
                Side::Unsupported,
            )?);
        }
    }
    let cf_prefix = document["overrides"].as_str().unwrap_or("overrides");
    vesta_protocol::pack::catalogue::RelativePath::parse(cf_prefix)
        .map_err(super::super::PackError::from)?;
    let mut blobs = Vec::new();
    for (name, bytes) in files {
        let path = name
            .strip_prefix("client-overrides/")
            .or_else(|| name.strip_prefix(&format!("{cf_prefix}/")));
        if let Some(path) = path {
            // Client overrides take precedence over shared overrides, without silently
            // overwriting a provider entry whose identity would then be incorrect.
            let source = EntrySource::Upload {
                sha256: digest(&bytes),
            };
            if let Some(old) = manifest.entries.iter_mut().find(|e| e.path == path) {
                if !matches!(old.source, EntrySource::Upload { .. }) {
                    return Err(invalid());
                }
                old.source = source;
            } else {
                manifest.entries.push(entry(
                    format!("override-{}", manifest.entries.len()),
                    path.into(),
                    source,
                    true,
                    Side::Unsupported,
                )?);
            }
            blobs.push(bytes);
        }
    }
    validate_manifest(&manifest)?;
    Ok((manifest, blobs))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    struct Providers;
    impl ProviderClient for Providers {
        fn resolve(
            &self,
            provider: Provider,
            project: &str,
            version: &str,
        ) -> crate::content::Result<crate::content::ResolvedSource> {
            assert_eq!(
                (provider, project, version),
                (Provider::Curseforge, "123", "456")
            );
            Ok(crate::content::ResolvedSource {
                provider,
                filename: "example.jar".into(),
                download_url: "https://edge.forgecdn.net/example.jar".into(),
                expected_hash: crate::content::ExpectedHash::Sha1("a".repeat(40)),
            })
        }
    }
    fn archive(rows: &[(&str, &[u8])]) -> Vec<u8> {
        let mut z = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for (name, bytes) in rows {
            z.start_file(*name, zip::write::SimpleFileOptions::default())
                .unwrap();
            z.write_all(bytes).unwrap();
        }
        z.finish().unwrap().into_inner()
    }
    #[test]
    fn mrpack_preserves_optional_side_and_embeds_overrides_without_trusting_urls() {
        let document = serde_json::json!({"formatVersion":1,"game":"minecraft","name":"Imported","versionId":"2.0","dependencies":{"minecraft":"1.21.1","fabric-loader":"0.16.10"},"files":[{"path":"mods/example.jar","hashes":{"sha512":"a".repeat(128)},"downloads":["http://127.0.0.1/private"],"env":{"client":"optional","server":"required"}}]});
        let bytes = archive(&[
            (
                "modrinth.index.json",
                &serde_json::to_vec(&document).unwrap(),
            ),
            ("overrides/config/client.toml", b"foo=true"),
        ]);
        let (m, blobs) = parse_archive(&bytes, &Providers, |_| Ok(None)).unwrap();
        assert_eq!(m.loader, ClientLoader::Fabric);
        assert!(!m.entries[0].required);
        assert_eq!(m.entries[0].environment.server, Side::Required);
        assert!(matches!(m.entries[0].source, EntrySource::Manual { .. }));
        assert_eq!(blobs, vec![b"foo=true".to_vec()]);
    }
    #[test]
    fn curseforge_import_pins_ids_and_uses_the_provider_filename() {
        let document=br#"{"manifestType":"minecraftModpack","manifestVersion":1,"name":"CF","version":"1.2","minecraft":{"version":"1.21.1","modLoaders":[{"id":"forge-52.0.0","primary":true}]},"files":[{"projectID":123,"fileID":456,"required":true}]}"#;
        let (m, _) = parse_archive(
            &archive(&[("manifest.json", document)]),
            &Providers,
            |_| unreachable!(),
        )
        .unwrap();
        assert_eq!(m.entries[0].path, "mods/example.jar");
        assert_eq!(m.loader_version.as_deref(), Some("52.0.0"));
    }
    #[test]
    fn archive_traversal_and_non_pack_archives_refuse() {
        assert!(parse_archive(&archive(&[("../secret", b"x")]), &Providers, |_| Ok(None)).is_err());
        assert!(parse_archive(&archive(&[("hello.txt", b"x")]), &Providers, |_| Ok(None)).is_err());
    }
}
