//! Independent player-offering clock. Custom publication history is never repurposed.
use super::workspace::{ClientPackConfig, ClientPackMode, InstallSource};
use super::{
    ArtifactFormat, PackArtifacts, PackError, PackRelease, PublishedArtifact, ReleaseError,
};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::io::{Cursor, Read, Write};
use vesta_protocol::pack::catalogue::{
    MAX_REVISION, ManifestMetadata, Platform, PlatformPack, Sha256,
};
use vesta_state::StateDatabase;

type Result<T> = std::result::Result<T, ReleaseError>;
#[derive(Serialize, Deserialize)]
struct Offering {
    key: String,
    revision: u64,
    minimum: u64,
    source_minimum: u64,
    version: String,
    format: String,
    sha256: String,
    size: u64,
    at: i64,
}
pub(super) fn initialize(db: &StateDatabase) -> Result<()> {
    db.write(|c| c.execute_batch("CREATE TABLE IF NOT EXISTS player_pack_offering(server_id TEXT PRIMARY KEY, body TEXT NOT NULL);").map_err(Into::into))?;
    Ok(())
}
fn unavailable() -> ReleaseError {
    PackError::Unavailable.into()
}
pub(super) fn offered(
    db: &StateDatabase,
    artifacts: &PackArtifacts,
) -> Result<Option<PackRelease>> {
    Ok(db.write_with(|t| offered_in(t.connection(), db, artifacts))?)
}
fn offered_in(
    c: &rusqlite::Connection,
    db: &StateDatabase,
    artifacts: &PackArtifacts,
) -> Result<Option<PackRelease>> {
    let configured: bool = c.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='client_pack_config')",
        [],
        |r| r.get(0),
    )?;
    if !configured {
        return super::releases::current(c, db.server_id());
    }
    let raw: Option<String> = c
        .query_row(
            "SELECT body FROM client_pack_config WHERE server_id=?",
            [db.server_id()],
            |r| r.get(0),
        )
        .optional()?;
    let config: ClientPackConfig = raw
        .map(|r| serde_json::from_str(&r).map_err(|_| unavailable()))
        .transpose()?
        .unwrap_or_default();
    if config.mode == ClientPackMode::None {
        return Ok(None);
    }
    let old: Option<String> = c
        .query_row(
            "SELECT body FROM player_pack_offering WHERE server_id=?",
            [db.server_id()],
            |r| r.get(0),
        )
        .optional()?;
    let old: Option<Offering> = old
        .map(|s| serde_json::from_str(&s).map_err(|_| unavailable()))
        .transpose()?;
    let (key, version, base_revision, source_minimum, provider, base) =
        if config.mode == ClientPackMode::Custom {
            let Some(release) = super::releases::current(c, db.server_id())? else {
                return Ok(None);
            };
            (
                format!(
                    "custom:{}:{}",
                    release.revision(),
                    release.artifact().sha256().to_hex()
                ),
                release.display_version().to_owned(),
                release.revision(),
                release.minimum_revision(),
                None,
                Some(release.artifact().clone()),
            )
        } else {
            let link = super::workspace::server_link::read_link(c, db.server_id())?
                .ok_or_else(unavailable)?;
            let Some(installed) = link.installed else {
                return Ok(None);
            };
            let source = serde_json::to_string(&installed.source).map_err(|_| unavailable())?;
            let provider = match installed.source {
                InstallSource::Provider {
                    provider,
                    project_id,
                    version_id,
                } => Some(
                    PlatformPack::new(
                        match provider.as_str() {
                            "modrinth" => Platform::Modrinth,
                            "curseforge" => Platform::Curseforge,
                            _ => return Err(unavailable()),
                        },
                        &project_id,
                        &version_id,
                        None,
                    )
                    .map_err(PackError::from)?,
                ),
                _ => None,
            };
            (
                format!("server:{source}"),
                installed.display_version,
                super::releases::current(c, db.server_id())?.map_or(1, |r| r.revision() + 1),
                0,
                provider,
                None,
            )
        };
    let legacy_minimum =
        super::releases::current(c, db.server_id())?.map_or(1, |r| r.minimum_revision());
    let same = old.as_ref().is_some_and(|o| o.key == key);
    let revision = if same {
        old.as_ref().unwrap().revision
    } else {
        base_revision.max(old.as_ref().map_or(1, |o| o.revision + 1))
    };
    if revision > MAX_REVISION {
        return Err(ReleaseError::RevisionExhausted);
    }
    let custom_required: bool = if config.mode == ClientPackMode::Custom {
        c.query_row(
            "SELECT required_update FROM pack_publication_history WHERE server_id=? AND revision=?",
            params![db.server_id(), base_revision as i64],
            |r| r.get(0),
        )
        .optional()?
        .unwrap_or(source_minimum == base_revision)
    } else {
        false
    };
    let required = config.mode == ClientPackMode::ServerModpack && config.require_update
        || config.mode == ClientPackMode::Custom
            && (custom_required
                || old.as_ref().is_some_and(|o| {
                    source_minimum > o.source_minimum && (source_minimum > 1 || same)
                }));
    let minimum = if required {
        revision
    } else {
        old.as_ref().map_or(legacy_minimum, |o| o.minimum)
    };
    let (format, hash, size) = if same {
        let o = old.as_ref().unwrap();
        (format(&o.format)?, o.sha256.clone(), o.size)
    } else {
        let (kind, bytes) = if let Some(reference) = provider {
            let metadata = ManifestMetadata::new("Server modpack", &version, revision, minimum)
                .map_err(PackError::from)?;
            let p = super::PreparedPack::platform(reference, metadata)?;
            let super::configured::PreparedContent::Platform { bytes, .. } = p.content else {
                return Err(unavailable());
            };
            (ArtifactFormat::Catalogue, bytes.to_vec())
        } else if let Some(base) = base {
            let loaded = artifacts.load(&base, super::MAX_ARTIFACT_BYTES)?;
            (
                base.format(),
                rebase(base.format(), loaded.bytes(), revision, minimum)?,
            )
        } else {
            let source = key.strip_prefix("server:").ok_or_else(unavailable)?;
            let bytes: Vec<u8> = c
                .query_row(
                    "SELECT archive FROM server_modpack_delivery WHERE server_id=? AND source=?",
                    params![db.server_id(), source],
                    |r| r.get(0),
                )
                .optional()?
                .ok_or_else(unavailable)?;
            (
                ArtifactFormat::Vesta,
                rebase(ArtifactFormat::Vesta, &bytes, revision, minimum)?,
            )
        };
        let hash = super::workspace::digest(&bytes);
        let artifact = artifacts
            .publish_tracked(
                kind,
                &bytes,
                &Sha256::parse(&hash).map_err(PackError::from)?,
            )?
            .0;
        (kind, hash, artifact.size())
    };
    let offering = Offering {
        key,
        revision,
        minimum: minimum.max(old.as_ref().map_or(legacy_minimum, |o| o.minimum)),
        source_minimum,
        version,
        format: format.extension().into(),
        sha256: hash,
        size,
        at: db.clock().millis(),
    };
    c.execute(
        "INSERT OR REPLACE INTO player_pack_offering VALUES(?,?)",
        params![
            db.server_id(),
            serde_json::to_string(&offering).map_err(|_| unavailable())?
        ],
    )?;
    Ok(Some(PackRelease {
        revision: offering.revision,
        minimum_revision: offering.minimum,
        display_version: offering.version,
        artifact: PublishedArtifact::from_parts(
            format,
            Sha256::parse(&offering.sha256).map_err(PackError::from)?,
            offering.size,
        ),
        published_at: offering.at,
    }))
}
fn format(value: &str) -> Result<ArtifactFormat> {
    match value {
        "vestapack" => Ok(ArtifactFormat::Vesta),
        "catalogue" => Ok(ArtifactFormat::Catalogue),
        "zip" => Ok(ArtifactFormat::Zip),
        "mrpack" => Ok(ArtifactFormat::Mrpack),
        _ => Err(unavailable()),
    }
}
/// Only locally built/verified immutable bytes enter this projection.
fn rebase(format: ArtifactFormat, bytes: &[u8], revision: u64, minimum: u64) -> Result<Vec<u8>> {
    if format == ArtifactFormat::Catalogue {
        let mut value: serde_json::Value =
            serde_json::from_slice(bytes).map_err(|_| unavailable())?;
        value["revision"] = revision.into();
        value["minimumRevision"] = minimum.into();
        return serde_json::to_vec(&value).map_err(|_| unavailable());
    }
    if format != ArtifactFormat::Vesta {
        return Ok(bytes.to_vec());
    }
    let mut zip = zip::ZipArchive::new(Cursor::new(bytes)).map_err(|_| unavailable())?;
    let mut out = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for n in 0..zip.len() {
        let mut entry = zip.by_index(n).map_err(|_| unavailable())?;
        let mut body = Vec::new();
        entry.read_to_end(&mut body).map_err(|_| unavailable())?;
        if entry.name() == vesta_protocol::pack::INDEX_NAME {
            let mut value: serde_json::Value =
                serde_json::from_slice(&body).map_err(|_| unavailable())?;
            value["revision"] = revision.into();
            value["minimumRevision"] = minimum.into();
            body = serde_json::to_vec(&value).map_err(|_| unavailable())?;
        }
        out.start_file(entry.name(), zip::write::SimpleFileOptions::default())
            .map_err(|_| unavailable())?;
        out.write_all(&body).map_err(|_| unavailable())?;
    }
    Ok(out.finish().map_err(|_| unavailable())?.into_inner())
}
