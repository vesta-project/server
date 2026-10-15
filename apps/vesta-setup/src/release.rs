use base64::Engine as _;
use ring::signature::{ED25519, UnparsedPublicKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

const MAX_MANIFEST: u64 = 16 * 1024;
const MAX_SIGNATURE: u64 = 256;
const SETUP_MAX: u64 = 64 * 1024 * 1024;
const FILE_MAX: u64 = 512 * 1024 * 1024;
const TOTAL_MAX: u64 = 1024 * 1024 * 1024;
const STREAM_CHUNK: usize = 64 * 1024;

#[derive(Clone, Debug)]
pub struct Source {
    pub local: Option<PathBuf>,
    pub base_url: Option<String>,
    pub manifest_sha256: String,
    pub release_key: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub schema: u8,
    pub version: String,
    pub target: String,
    pub files: Vec<Artifact>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Artifact {
    pub name: String,
    pub sha256: String,
    pub size: u64,
}

#[derive(Clone, Debug)]
enum ArtifactSource {
    Local(PathBuf),
    Remote(String),
}

#[derive(Clone, Debug)]
pub struct Release {
    pub manifest: Manifest,
    verified_manifest: Manifest,
    manifest_hash: String,
    source: ArtifactSource,
}

pub fn load(source: Source) -> Result<Release, String> {
    let pin = decode_hex_exact(&source.manifest_sha256, 32, "invalid manifest hash")?;
    let key = decode_hex_exact(&source.release_key, 32, "invalid release key")?;
    let (artifact_source, manifest_bytes, signature_bytes) = match (source.local, source.base_url) {
        (Some(local), None) => {
            validate_local_directory(&local)?;
            let manifest_path = local.join("manifest.json");
            let signature_path = local.join("manifest.sig");
            let manifest =
                read_local_bounded(&manifest_path, MAX_MANIFEST, "manifest unavailable")?;
            let signature =
                read_local_bounded(&signature_path, MAX_SIGNATURE, "signature unavailable")?;
            (ArtifactSource::Local(local), manifest, signature)
        }
        (None, Some(base)) => {
            validate_base_url(&base)?;
            let agent = http_agent();
            let manifest_url = format!("{base}/manifest.json");
            let signature_url = format!("{base}/manifest.sig");
            let manifest = fetch_bounded(
                &agent,
                &manifest_url,
                MAX_MANIFEST,
                "manifest download failed",
            )?;
            let signature = fetch_bounded(
                &agent,
                &signature_url,
                MAX_SIGNATURE,
                "signature download failed",
            )?;
            (ArtifactSource::Remote(base), manifest, signature)
        }
        _ => return Err("invalid source".into()),
    };

    let actual = Sha256::digest(&manifest_bytes);
    if !constant_time_equal(&actual, &pin) {
        return Err("manifest hash mismatch".into());
    }
    let signature = base64::engine::general_purpose::STANDARD
        .decode(
            signature_bytes
                .iter()
                .copied()
                .map(char::from)
                .collect::<String>()
                .trim(),
        )
        .map_err(|_| "invalid release signature".to_string())?;
    if signature.len() != 64 {
        return Err("invalid release signature".into());
    }
    UnparsedPublicKey::new(&ED25519, key)
        .verify(&manifest_bytes, &signature)
        .map_err(|_| "release signature verification failed".to_string())?;

    let manifest: Manifest =
        serde_json::from_slice(&manifest_bytes).map_err(|_| "invalid manifest".to_string())?;
    validate_manifest(&manifest)?;
    Ok(Release {
        verified_manifest: manifest.clone(),
        manifest_hash: hex::encode(pin),
        manifest,
        source: artifact_source,
    })
}

impl Release {
    pub fn manifest_hash(&self) -> &str {
        &self.manifest_hash
    }
    pub(crate) fn ensure_intact(&self) -> Result<(), String> {
        if self.manifest != self.verified_manifest {
            return Err("release changed after signature verification".into());
        }
        Ok(())
    }
    pub fn install(
        &self,
        root: &Path,
        progress: &mut dyn FnMut(&str, u64, u64),
    ) -> Result<(), String> {
        self.ensure_intact()?;
        validate_existing_path_chain(root)?;
        let bin = root.join("bin");
        match fs::create_dir(&bin) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(_) => return Err("install directory unavailable".into()),
        }
        validate_existing_path_chain(&bin)?;
        if !fs::metadata(&bin)
            .map_err(|_| "install directory unavailable")?
            .is_dir()
        {
            return Err("install directory unavailable".into());
        }
        for artifact in &self.manifest.files {
            let destination = bin.join(&artifact.name);
            match fs::symlink_metadata(&destination) {
                Ok(meta) => {
                    if is_link_or_reparse(&meta) || !meta.is_file() {
                        return Err("destination exists and is not a regular file".into());
                    }
                    let (hash, size) = hash_file(&destination)?;
                    if size == artifact.size && hex::encode(hash) == artifact.sha256 {
                        continue;
                    }
                    return Err("destination already exists with different contents".into());
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => return Err("destination cannot be inspected".into()),
            }

            let (mut input, response_guard): (
                Box<dyn Read>,
                Option<ureq::http::Response<ureq::Body>>,
            ) = match &self.source {
                ArtifactSource::Local(directory) => {
                    let path = directory.join(&artifact.name);
                    validate_local_file(&path, directory)?;
                    let file = File::open(&path).map_err(|_| "artifact unavailable")?;
                    (Box::new(file), None)
                }
                ArtifactSource::Remote(base) => {
                    let url = format!("{base}/{}", artifact.name);
                    let response = http_agent()
                        .get(&url)
                        .call()
                        .map_err(|_| "artifact download failed")?;
                    if response.status() != 200 {
                        return Err("artifact download failed".into());
                    }
                    (Box::new(std::io::empty()), Some(response))
                }
            };
            let (temp_path, mut output) = create_temp(&destination)?;
            let copy_result = if let Some(mut response) = response_guard {
                let reader = response
                    .body_mut()
                    .with_config()
                    .limit(artifact.size)
                    .reader();
                copy_artifact(reader, &mut output, artifact, progress)
            } else {
                copy_artifact(&mut input, &mut output, artifact, progress)
            };
            let result = copy_result.and_then(|_| {
                output
                    .sync_all()
                    .map_err(|_| "artifact sync failed".to_string())?;
                drop(output);
                set_executable(&temp_path)?;
                match fs::hard_link(&temp_path, &destination) {
                    Ok(()) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                        return Err("destination already exists".into());
                    }
                    Err(_) => return Err("artifact publish failed".into()),
                }
                fs::remove_file(&temp_path).map_err(|_| "temporary cleanup failed".to_string())?;
                sync_directory(&bin)?;
                Ok(())
            });
            if result.is_err() {
                let _ = fs::remove_file(&temp_path);
            }
            result?;
        }
        Ok(())
    }
}

fn validate_manifest(manifest: &Manifest) -> Result<(), String> {
    if manifest.schema != 1
        || manifest.version.is_empty()
        || manifest.version.len() > 64
        || !manifest
            .version
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'+' | b'-'))
    {
        return Err("invalid manifest version or schema".into());
    }
    let target = crate::platform::target()?;
    if manifest.target != target {
        return Err("release target does not match this platform".into());
    }
    #[cfg(target_os = "windows")]
    let expected = ["vesta-setup.exe", "vesta-agent.exe", "vesta-supervisor.exe"];
    #[cfg(not(target_os = "windows"))]
    let expected = ["vesta-setup", "vesta-agent", "vesta-supervisor"];
    if manifest.files.len() != expected.len()
        || manifest
            .files
            .iter()
            .zip(expected)
            .any(|(f, n)| f.name != n)
    {
        return Err("manifest artifact list is invalid".into());
    }
    let mut total = 0u64;
    for (i, file) in manifest.files.iter().enumerate() {
        decode_hex_exact(&file.sha256, 32, "invalid artifact hash")?;
        let limit = if i == 0 { SETUP_MAX } else { FILE_MAX };
        if file.size == 0 || file.size > limit {
            return Err("artifact size is invalid".into());
        }
        total = total
            .checked_add(file.size)
            .ok_or("artifact sizes overflow")?;
    }
    if total > TOTAL_MAX {
        return Err("release is too large".into());
    }
    Ok(())
}

fn decode_hex_exact(value: &str, bytes: usize, error: &'static str) -> Result<Vec<u8>, String> {
    if value.len() != bytes * 2 || !value.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(error.into());
    }
    hex::decode(value).map_err(|_| error.into())
}

fn validate_base_url(value: &str) -> Result<(), String> {
    if !value.starts_with("https://")
        || value.contains('?')
        || value.contains('#')
        || value.contains('@')
        || value.ends_with('/')
    {
        return Err("invalid HTTPS release URL".into());
    }
    let authority_path = &value[8..];
    let authority_end = authority_path.find('/').unwrap_or(authority_path.len());
    let authority = &authority_path[..authority_end];
    if authority.is_empty() {
        return Err("invalid HTTPS release URL".into());
    }
    let uri: ureq::http::Uri = value
        .parse()
        .map_err(|_| "invalid HTTPS release URL".to_string())?;
    if uri.scheme_str() != Some("https") || uri.authority().is_none() || uri.path().contains("..") {
        return Err("invalid HTTPS release URL".into());
    }
    Ok(())
}

fn http_agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .proxy(None)
        .max_redirects(0)
        .timeout_global(Some(Duration::from_secs(60)))
        .build()
        .into()
}

fn fetch_bounded(
    agent: &ureq::Agent,
    url: &str,
    max: u64,
    error: &'static str,
) -> Result<Vec<u8>, String> {
    let mut response = agent.get(url).call().map_err(|_| error.to_string())?;
    if response.status() != 200 {
        return Err(error.into());
    }
    let bytes = response
        .body_mut()
        .with_config()
        .limit(max)
        .read_to_vec()
        .map_err(|_| error.to_string())?;
    if bytes.len() as u64 > max {
        return Err(error.into());
    }
    Ok(bytes)
}

fn read_local_bounded(path: &Path, max: u64, error: &'static str) -> Result<Vec<u8>, String> {
    validate_existing_path_chain(path)?;
    let meta = fs::symlink_metadata(path).map_err(|_| error.to_string())?;
    if is_link_or_reparse(&meta) || !meta.is_file() || meta.len() > max {
        return Err(error.into());
    }
    let file = File::open(path).map_err(|_| error.to_string())?;
    let mut bytes = Vec::with_capacity(meta.len() as usize);
    file.take(max + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| error.to_string())?;
    if bytes.len() as u64 > max {
        return Err(error.into());
    }
    Ok(bytes)
}

fn validate_local_directory(path: &Path) -> Result<(), String> {
    validate_existing_path_chain(path)?;
    if !fs::metadata(path)
        .map_err(|_| "local release directory unavailable")?
        .is_dir()
    {
        return Err("local release directory unavailable".into());
    }
    Ok(())
}

fn validate_local_file(path: &Path, base: &Path) -> Result<(), String> {
    validate_existing_path_chain(base)?;
    let meta = fs::symlink_metadata(path).map_err(|_| "artifact unavailable")?;
    if is_link_or_reparse(&meta) || !meta.is_file() || meta.len() == 0 {
        return Err("artifact unavailable".into());
    }
    Ok(())
}

fn validate_existing_path_chain(path: &Path) -> Result<(), String> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|_| "path unavailable")?
            .join(path)
    };
    let mut current = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::Prefix(p) => current.push(p.as_os_str()),
            Component::RootDir => current.push(component.as_os_str()),
            Component::CurDir => continue,
            Component::ParentDir => return Err("path contains parent traversal".into()),
            Component::Normal(part) => current.push(part),
        }
        match fs::symlink_metadata(&current) {
            Ok(meta) if is_link_or_reparse(&meta) => {
                return Err("path contains a link or reparse point".into());
            }
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err("path unavailable".into());
            }
            Err(_) => return Err("path unavailable".into()),
        }
    }
    Ok(())
}

fn is_link_or_reparse(meta: &fs::Metadata) -> bool {
    if meta.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        return meta.file_attributes() & 0x400 != 0;
    }
    #[allow(unreachable_code)]
    false
}

fn hash_file(path: &Path) -> Result<(Vec<u8>, u64), String> {
    let mut file = File::open(path).map_err(|_| "existing file cannot be read")?;
    let mut hasher = Sha256::new();
    let mut total = 0u64;
    let mut buffer = [0u8; STREAM_CHUNK];
    loop {
        let n = file
            .read(&mut buffer)
            .map_err(|_| "existing file cannot be read")?;
        if n == 0 {
            break;
        }
        total = total.saturating_add(n as u64);
        hasher.update(&buffer[..n]);
    }
    Ok((hasher.finalize().to_vec(), total))
}

fn create_temp(destination: &Path) -> Result<(PathBuf, File), String> {
    for attempt in 0..32u32 {
        let name = format!(
            ".vesta-setup-{}-{}-{attempt}.tmp",
            std::process::id(),
            TEMP_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        );
        let path = destination
            .parent()
            .ok_or("invalid install destination")?
            .join(name);
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        match options.open(&path) {
            Ok(file) => return Ok((path, file)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(_) => return Err("could not create temporary artifact".into()),
        }
    }
    Err("could not create temporary artifact".into())
}

static TEMP_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn copy_artifact(
    mut reader: impl Read,
    output: &mut File,
    artifact: &Artifact,
    progress: &mut dyn FnMut(&str, u64, u64),
) -> Result<(), String> {
    let mut hasher = Sha256::new();
    let mut total = 0u64;
    let mut buffer = [0u8; STREAM_CHUNK];
    loop {
        let n = reader
            .read(&mut buffer)
            .map_err(|_| "artifact read failed")?;
        if n == 0 {
            break;
        }
        total = total.checked_add(n as u64).ok_or("artifact too large")?;
        if total > artifact.size {
            return Err("artifact size mismatch".into());
        }
        output
            .write_all(&buffer[..n])
            .map_err(|_| "artifact write failed")?;
        hasher.update(&buffer[..n]);
        progress(&artifact.name, total, artifact.size);
    }
    if total != artifact.size || hex::encode(hasher.finalize()) != artifact.sha256 {
        return Err("artifact verification failed".into());
    }
    Ok(())
}

fn set_executable(path: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))
            .map_err(|_| "artifact permission update failed".into())?;
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}

fn sync_directory(path: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        File::open(path)
            .and_then(|f| f.sync_all())
            .map_err(|_| "install directory sync failed".into())?;
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}

fn constant_time_equal(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right)
        .fold(0u8, |diff, (a, b)| diff | (a ^ b))
        == 0
}
