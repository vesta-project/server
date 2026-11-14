//! Host-level review of a server modpack before the server exists (create.md
//! `kind:"modpack"`). It runs the same steps as a server's own provider pack
//! install review (provider version document bound to its project, exact file
//! resolution, the pack's runtime manifest read through a ranged remote zip,
//! the same size notice) and, for uploaded archives, the same archive policy
//! and manifest parsers. Minecraft version and loader always come from the
//! pack. Nothing is written to any game directory.
use super::*;
use crate::content::{self, ContentError, Provider, ProviderClient};
use std::{
    io::Read,
    path::Path,
    time::{Duration, Instant},
};

/// One review notice, kept verbatim from the pack review.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreatePackIssue {
    pub code: String,
    pub message: String,
    pub blocking: bool,
}

/// What the pack itself declares, plus its review notices.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreatePackReview {
    pub name: String,
    pub minecraft_version: String,
    /// `vanilla`, `fabric`, `quilt`, `forge` or `neoforge`.
    pub loader: String,
    pub loader_version: Option<String>,
    /// The pack supports only Minecraft clients.
    pub client_only: bool,
    pub issues: Vec<CreatePackIssue>,
    /// Pack bytes to download, when the provider says.
    pub content_bytes: Option<u64>,
    /// Identity of the reviewed archive: SHA-256 of an uploaded file, or of the
    /// provider's exact file binding (name and published hash).
    pub source_digest: String,
}

fn loader_id(loader: &ClientLoader) -> &'static str {
    match loader {
        ClientLoader::None => "vanilla",
        other => other.id(),
    }
}

/// Fixed create codes: `provider_not_configured`, `pack_invalid`, else
/// `provider_unavailable`.
fn content_failure(error: ContentError) -> &'static str {
    match error {
        ContentError::ProviderNotConfigured => "provider_not_configured",
        ContentError::InvalidMetadata
        | ContentError::InvalidRequest
        | ContentError::UnknownSource
        | ContentError::UnsafeFilename
        | ContentError::DisallowedUrl
        | ContentError::UrlRefusal { .. }
        | ContentError::ModsUnsupported
        | ContentError::PluginsUnsupported => "pack_invalid",
        _ => "provider_unavailable",
    }
}
fn release_failure(error: &ReleaseError) -> &'static str {
    match error.code() {
        "content_provider_not_configured" => "provider_not_configured",
        "content_provider_unavailable" | "content_pack_download_failed" => "provider_unavailable",
        _ => "pack_invalid",
    }
}

fn manifest_review(
    manifest: PackManifest,
    client_only: bool,
    issues: Vec<CreatePackIssue>,
    content_bytes: Option<u64>,
    source_digest: String,
) -> CreatePackReview {
    CreatePackReview {
        name: manifest.name,
        minecraft_version: manifest.minecraft_version,
        loader: loader_id(&manifest.loader).into(),
        loader_version: manifest.loader_version,
        client_only,
        issues,
        content_bytes,
        source_digest,
    }
}
fn notice(warning: InstallWarning) -> CreatePackIssue {
    CreatePackIssue {
        code: warning.code,
        message: warning.detail,
        blocking: false,
    }
}

/// Review an immutable provider version of a modpack.
pub fn review_create_provider_pack(
    provider: &str,
    project_id: &str,
    version_id: &str,
    providers: &content::UreqProviderClient,
) -> std::result::Result<CreatePackReview, &'static str> {
    let platform = match provider {
        "modrinth" => Provider::Modrinth,
        "curseforge" => Provider::Curseforge,
        _ => return Err("pack_invalid"),
    };
    let providers = providers
        .with_deadline(Instant::now() + Duration::from_secs(60))
        .map_err(content_failure)?;
    let document = providers
        .pack_document(platform, project_id, Some(version_id), false)
        .map_err(content_failure)?;
    // The immutable version must belong to the named project.
    let owner = match platform {
        Provider::Modrinth => document["project_id"].as_str().map(str::to_owned),
        Provider::Curseforge => document["modId"].as_u64().map(|id| id.to_string()),
    };
    if owner.as_deref() != Some(project_id) {
        return Err("pack_invalid");
    }
    let source = providers
        .resolve(platform, project_id, version_id)
        .map_err(content_failure)?;
    let remote = content::RemoteZip::open(&source).map_err(content_failure)?;
    let manifest = if platform == Provider::Curseforge {
        super::import::parse_curseforge_runtime_reader(remote)
    } else {
        super::import::parse_modrinth_runtime_reader(remote)
    }
    .map_err(|e| release_failure(&e))?;
    // Same size estimate and notice as a server's provider pack install plan.
    let mut size = document["fileLength"].as_u64().unwrap_or_else(|| {
        document["files"]
            .as_array()
            .map(|files| files.iter().filter_map(|file| file["size"].as_u64()).sum())
            .unwrap_or(0)
    });
    if let Some(id) = document["serverPackFileId"].as_u64().filter(|id| *id > 0) {
        let server = providers
            .pack_document(Provider::Curseforge, project_id, Some(&id.to_string()), false)
            .map_err(content_failure)?;
        size = server["fileLength"].as_u64().unwrap_or(size);
    }
    let issues = vec![notice(format!("pack_size_warning:{size}").into())];
    // Modrinth declares server support on the project.
    let client_only = platform == Provider::Modrinth
        && providers
            .pack_document(platform, project_id, None, false)
            .map_err(content_failure)?["server_side"]
            .as_str()
            == Some("unsupported");
    let binding = format!(
        "{provider}\0{project_id}\0{version_id}\0{}\0{:?}",
        source.filename, source.expected_hash
    );
    Ok(manifest_review(
        manifest,
        client_only,
        issues,
        Some(size),
        digest(binding.as_bytes()),
    ))
}

/// Review an uploaded pack archive (`.mrpack` or a CurseForge pack zip) that
/// already passed the upload archive safety checks.
pub fn review_create_uploaded_pack(
    path: &Path,
) -> std::result::Result<CreatePackReview, &'static str> {
    let open = || std::fs::File::open(path).map_err(|_| "pack_invalid");
    let size = open()?.metadata().map_err(|_| "pack_invalid")?.len();
    let mut hasher = <sha2::Sha256 as sha2::Digest>::new();
    std::io::copy(&mut open()?, &mut hasher).map_err(|_| "pack_invalid")?;
    let source_digest = hex::encode(sha2::Digest::finalize(hasher));
    let issues = super::install::archive_policy::inspect(open()?, size, "upload")
        .map_err(|e| release_failure(&e))?
        .into_iter()
        .map(|w| CreatePackIssue {
            message: format!("{}: {} ({})", w.rule, w.item, w.detail),
            code: w.code,
            blocking: false,
        })
        .collect();
    let mut zip = zip::ZipArchive::new(open()?).map_err(|_| "pack_invalid")?;
    let modrinth = zip.by_name("modrinth.index.json").is_ok();
    let curseforge = !modrinth && zip.by_name("manifest.json").is_ok();
    let manifest = if modrinth {
        super::import::parse_modrinth_runtime_reader(open()?)
    } else if curseforge {
        super::import::parse_curseforge_runtime_reader(open()?)
    } else {
        return Err("pack_invalid");
    }
    .map_err(|e| release_failure(&e))?;
    // A Modrinth pack whose every file is unsupported on servers is client-only.
    let client_only = modrinth && {
        let mut index = Vec::new();
        zip.by_name("modrinth.index.json")
            .map_err(|_| "pack_invalid")?
            .take(1024 * 1024)
            .read_to_end(&mut index)
            .map_err(|_| "pack_invalid")?;
        let index: serde_json::Value =
            serde_json::from_slice(&index).map_err(|_| "pack_invalid")?;
        index["files"].as_array().is_some_and(|files| {
            !files.is_empty()
                && files
                    .iter()
                    .all(|f| f["env"]["server"].as_str() == Some("unsupported"))
        })
    };
    Ok(manifest_review(manifest, client_only, issues, None, source_digest))
}

/// Archive limits for an uploaded server pack (create.md).
pub const CREATE_UPLOAD_MAX_EXTRACTED: u64 = 16 * 1024 * 1024 * 1024;
pub const CREATE_UPLOAD_MAX_FILES: usize = 100_000;

/// Structural safety of an uploaded archive: every entry a safe relative path,
/// a regular file or directory (no links or special files), at most 100000
/// files and 16 GiB extracted. `unsafe_archive` or `source_too_large`.
pub fn check_create_upload_archive(path: &Path) -> std::result::Result<u64, &'static str> {
    let file = std::fs::File::open(path).map_err(|_| "unsafe_archive")?;
    let mut zip = zip::ZipArchive::new(file).map_err(|_| "unsafe_archive")?;
    if zip.len() > CREATE_UPLOAD_MAX_FILES {
        return Err("source_too_large");
    }
    let mut total = 0u64;
    for index in 0..zip.len() {
        let entry = zip.by_index_raw(index).map_err(|_| "unsafe_archive")?;
        let name = entry.name();
        if name.contains('\\')
            || vesta_protocol::pack::catalogue::RelativePath::parse(name.trim_end_matches('/'))
                .is_err()
        {
            return Err("unsafe_archive");
        }
        // Unix type bits when present: only regular files and directories.
        if let Some(mode) = entry.unix_mode() {
            let kind = mode & 0o170000;
            if kind != 0 && kind != 0o100000 && kind != 0o040000 {
                return Err("unsafe_archive");
            }
        }
        if entry.is_symlink() {
            return Err("unsafe_archive");
        }
        total = total.checked_add(entry.size()).ok_or("source_too_large")?;
        if total > CREATE_UPLOAD_MAX_EXTRACTED {
            return Err("source_too_large");
        }
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn archive(entries: &[(&str, &[u8])], symlink: Option<&str>) -> tempfile::NamedTempFile {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        for (name, body) in entries {
            zip.start_file(*name, zip::write::SimpleFileOptions::default())
                .unwrap();
            zip.write_all(body).unwrap();
        }
        if let Some(name) = symlink {
            zip.add_symlink(name, "/etc/passwd", zip::write::SimpleFileOptions::default())
                .unwrap();
        }
        let bytes = zip.finish().unwrap().into_inner();
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(&bytes).unwrap();
        file
    }
    fn index(server: &str) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "formatVersion": 1, "game": "minecraft", "versionId": "1.0", "name": "Upload Pack",
            "dependencies": {"minecraft": "1.21.1", "fabric-loader": "0.16.9"},
            "files": [{"path": "mods/a.jar", "hashes": {"sha1": "a", "sha512": "b"},
                "env": {"client": "required", "server": server},
                "downloads": ["https://cdn.modrinth.com/data/x/a.jar"], "fileSize": 1}]
        }))
        .unwrap()
    }

    #[test]
    fn uploaded_mrpack_review_reads_the_pack_itself() {
        let file = archive(
            &[
                ("modrinth.index.json", &index("required")),
                ("overrides/config/a.txt", b"a"),
            ],
            None,
        );
        assert_eq!(
            check_create_upload_archive(file.path()),
            Ok(index("required").len() as u64 + 1)
        );
        let review = review_create_uploaded_pack(file.path()).unwrap();
        assert_eq!(review.name, "Upload Pack");
        assert_eq!(review.minecraft_version, "1.21.1");
        assert_eq!(review.loader, "fabric");
        assert_eq!(review.loader_version.as_deref(), Some("0.16.9"));
        assert!(!review.client_only);
        let bytes = std::fs::read(file.path()).unwrap();
        assert_eq!(review.source_digest, digest(&bytes));

        let client = archive(&[("modrinth.index.json", &index("unsupported"))], None);
        assert!(review_create_uploaded_pack(client.path()).unwrap().client_only);
        // Neither a Modrinth nor a CurseForge pack.
        let plain = archive(&[("mods/a.jar", b"x")], None);
        assert_eq!(review_create_uploaded_pack(plain.path()), Err("pack_invalid"));
    }

    #[test]
    fn unsafe_upload_archives_are_refused() {
        for name in ["../escape.txt", "/abs.txt", "a/../../b.txt", "a\\b.txt"] {
            let file = archive(&[("modrinth.index.json", b"{}"), (name, b"x")], None);
            assert_eq!(check_create_upload_archive(file.path()), Err("unsafe_archive"), "{name}");
        }
        let link = archive(&[("modrinth.index.json", b"{}")], Some("overrides/link"));
        assert_eq!(check_create_upload_archive(link.path()), Err("unsafe_archive"));
        let mut junk = tempfile::NamedTempFile::new().unwrap();
        junk.write_all(b"not a zip").unwrap();
        assert_eq!(check_create_upload_archive(junk.path()), Err("unsafe_archive"));
        let fine = archive(&[("modrinth.index.json", b"{}"), ("overrides/a.txt", b"xy")], None);
        assert_eq!(check_create_upload_archive(fine.path()), Ok(4));
    }
}
