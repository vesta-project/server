//! Host-owned Java discovery, probing and checksum-verified managed runtimes.
use crate::{
    config::HostConfiguration,
    identity::{self, BuiltinRole, Capability, Subject},
    software::{Result, SoftwareError},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    fs,
    io::Read,
    path::{Component, Path, PathBuf},
    time::Duration,
};
use vesta_state::StateDatabase;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct JavaRuntime {
    pub id: String,
    pub path: String,
    pub version: String,
    pub major: u32,
    pub vendor: String,
    pub arch: String,
    pub managed: bool,
}
#[derive(Serialize)]
pub struct Inventory {
    pub runtimes: Vec<JavaRuntime>,
    pub selected: Option<JavaRuntime>,
    pub custom_paths_allowed: bool,
    pub managed_majors: Vec<u32>,
}
pub fn authorize(db: &StateDatabase, subject: &Subject) -> Result<()> {
    db.read_with(|s| -> Result<()> {
        let p = identity::policy_in(
            s.connection(),
            db.server_id(),
            db.clock().millis(),
            Some(subject),
        )
        .map_err(|_| SoftwareError::StateUnavailable)?;
        if p.holds_builtin(BuiltinRole::Owner)
            && p.check(Capability::ContentInstall, None).allowed()
        {
            Ok(())
        } else {
            Err(SoftwareError::Forbidden)
        }
    })
    .map_err(Into::into)
}
pub fn managed_root(config: &HostConfiguration) -> PathBuf {
    config.state().join("managed-java")
}
fn canonical(path: &Path) -> Result<PathBuf> {
    if !path.is_absolute() {
        return Err(SoftwareError::InvalidRequest);
    }
    fs::canonicalize(path).map_err(|_| SoftwareError::InvalidRequest)
}
fn java_leaf() -> &'static str {
    if cfg!(windows) { "java.exe" } else { "java" }
}
pub fn major(version: &str) -> Option<u32> {
    let mut parts = version.split(['.', '-', '+', '_']);
    let first = parts.next()?.parse::<u32>().ok()?;
    if first == 1 {
        parts.next()?.parse().ok()
    } else {
        Some(first)
    }
}
fn properties(text: &str, key: &str) -> Option<String> {
    text.lines().find_map(|line| {
        let (k, v) = line.trim().split_once(" = ")?;
        (k == key).then(|| v.trim().to_owned())
    })
}
pub async fn probe(path: &Path, managed: bool) -> Result<JavaRuntime> {
    use tokio::io::AsyncReadExt;
    let path = canonical(path)?;
    if !path.is_file() {
        return Err(SoftwareError::InvalidRequest);
    }
    let mut command = tokio::process::Command::new(&path);
    command
        .args(["-XshowSettings:properties", "-version"])
        .env_remove("JAVA_TOOL_OPTIONS")
        .env_remove("_JAVA_OPTIONS")
        .env_remove("JDK_JAVA_OPTIONS")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    #[cfg(windows)]
    command.creation_flags(0x08000000);
    let mut child = command
        .spawn()
        .map_err(|_| SoftwareError::RuntimeUnavailable)?;
    let stdout = child
        .stdout
        .take()
        .ok_or(SoftwareError::RuntimeUnavailable)?;
    let stderr = child
        .stderr
        .take()
        .ok_or(SoftwareError::RuntimeUnavailable)?;
    let result = tokio::time::timeout(Duration::from_secs(5), async {
        let read = |stream: tokio::process::ChildStdout| async move {
            let mut bytes = Vec::new();
            stream
                .take(32769)
                .read_to_end(&mut bytes)
                .await
                .map(|_| bytes)
        };
        let err = async {
            let mut bytes = Vec::new();
            stderr
                .take(32769)
                .read_to_end(&mut bytes)
                .await
                .map(|_| bytes)
        };
        let (out, err, status) = tokio::join!(read(stdout), err, child.wait());
        Ok::<_, std::io::Error>((out?, err?, status?))
    })
    .await
    .map_err(|_| SoftwareError::RuntimeUnavailable)?
    .map_err(|_| SoftwareError::RuntimeUnavailable)?;
    if !result.2.success() || result.0.len() > 32768 || result.1.len() > 32768 {
        return Err(SoftwareError::RuntimeUnavailable);
    }
    let text = format!(
        "{}\n{}",
        String::from_utf8_lossy(&result.0),
        String::from_utf8_lossy(&result.1)
    );
    let version = properties(&text, "java.version").ok_or(SoftwareError::RuntimeUnavailable)?;
    let major = major(&version)
        .filter(|m| (8..=99).contains(m))
        .ok_or(SoftwareError::RuntimeUnavailable)?;
    let path = path
        .to_str()
        .ok_or(SoftwareError::InvalidRequest)?
        .to_owned();
    Ok(JavaRuntime {
        id: hex::encode(Sha256::digest(path.as_bytes())),
        path,
        version,
        major,
        vendor: properties(&text, "java.vendor").unwrap_or_else(|| "Unknown".into()),
        arch: properties(&text, "os.arch").ok_or(SoftwareError::RuntimeUnavailable)?,
        managed,
    })
}
fn candidates(config: &HostConfiguration, selected: &Path) -> Vec<PathBuf> {
    let mut set = BTreeSet::new();
    set.insert(selected.to_owned());
    if let Some(home) = std::env::var_os("JAVA_HOME") {
        set.insert(PathBuf::from(home).join("bin").join(java_leaf()));
    }
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path).take(128) {
            if dir.is_absolute() {
                set.insert(dir.join(java_leaf()));
            }
        }
    }
    let mut roots = vec![managed_root(config)];
    #[cfg(windows)]
    for base in ["ProgramFiles", "ProgramFiles(x86)", "LOCALAPPDATA"] {
        if let Some(root) = std::env::var_os(base) {
            for vendor in [
                "Java",
                "Eclipse Adoptium",
                "Eclipse Foundation",
                "Microsoft",
                "Zulu",
                "Azul",
                "Oracle",
                "Programs/Eclipse Adoptium",
            ] {
                roots.push(PathBuf::from(&root).join(vendor));
            }
        }
    }
    #[cfg(target_os = "linux")]
    roots.extend(
        [
            "/usr/lib/jvm",
            "/usr/java",
            "/opt/java",
            "/opt/jdk",
            "/opt/microsoft",
            "/opt/zulu",
        ]
        .map(PathBuf::from),
    );
    #[cfg(target_os = "macos")]
    roots.extend(
        [
            "/Library/Java/JavaVirtualMachines",
            "/opt/homebrew/opt",
            "/usr/local/opt",
        ]
        .map(PathBuf::from),
    );
    if let Some(home) = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }) {
        roots.push(PathBuf::from(home).join(".jdks"));
    }
    for root in roots {
        for path in std::iter::once(root.clone()).chain(
            fs::read_dir(&root)
                .into_iter()
                .flatten()
                .take(64)
                .filter_map(|e| e.ok().map(|e| e.path())),
        ) {
            if root == managed_root(config)
                && path
                    .file_name()
                    .is_some_and(|n| n.to_string_lossy().starts_with('.'))
            {
                continue;
            }
            set.insert(path.join("bin").join(java_leaf()));
            set.insert(path.join("Contents/Home/bin").join(java_leaf()));
            // Managed archives keep the upstream top-level folder.
            if root == managed_root(config) && let Ok(entries) = fs::read_dir(path) {
                for e in entries.flatten().take(8) {
                    set.insert(e.path().join("bin").join(java_leaf()));
                    set.insert(e.path().join("Contents/Home/bin").join(java_leaf()));
                }
            }
        }
    }
    set.into_iter().filter(|p| p.is_file()).take(64).collect()
}
pub async fn inventory(config: &HostConfiguration, selected: &Path) -> Result<Inventory> {
    let root = fs::canonicalize(managed_root(config)).ok();
    let mut runtimes = Vec::new();
    let mut seen = BTreeSet::new();
    for path in candidates(config, selected) {
        if let Ok(path) = canonical(&path)
            && seen.insert(path.clone())
        {
            let managed = root.as_ref().is_some_and(|r| path.starts_with(r));
            if let Ok(runtime) = probe(&path, managed).await {
                runtimes.push(runtime);
            }
        }
    }
    let selected_path = canonical(selected).ok();
    let selected = runtimes
        .iter()
        .find(|r| selected_path.as_ref() == Some(&PathBuf::from(&r.path)))
        .cloned();
    let managed_majors = runtimes
        .iter()
        .filter(|r| r.managed)
        .map(|r| r.major)
        .collect();
    Ok(Inventory {
        runtimes,
        selected,
        custom_paths_allowed: !config.java_allowlist().is_empty(),
        managed_majors,
    })
}
pub async fn select(
    config: &HostConfiguration,
    selected: &Path,
    id: Option<&str>,
    custom: Option<&str>,
) -> Result<JavaRuntime> {
    match (id, custom) {
        (Some(id), None) => inventory(config, selected)
            .await?
            .runtimes
            .into_iter()
            .find(|r| r.id == id)
            .ok_or(SoftwareError::InvalidRequest),
        (None, Some(path)) => {
            let path = canonical(Path::new(path))?;
            if !config
                .java_allowlist()
                .iter()
                .filter_map(|root| canonical(root).ok())
                .any(|root| root.is_dir() && path.starts_with(root))
            {
                return Err(SoftwareError::Forbidden);
            }
            probe(&path, false).await
        }
        _ => Err(SoftwareError::InvalidRequest),
    }
}

fn trusted_url(value: &str) -> Result<url::Url> {
    let url = url::Url::parse(value).map_err(|_| SoftwareError::InvalidRequest)?;
    if url.scheme() != "https"
        || !url.username().is_empty()
        || url.password().is_some()
        || !matches!(
            url.host_str(),
            Some(
                "api.adoptium.net"
                    | "github.com"
                    | "release-assets.githubusercontent.com"
                    | "objects.githubusercontent.com"
            )
        )
    {
        return Err(SoftwareError::InvalidRequest);
    }
    Ok(url)
}
fn fetch(url: &str, limit: u64) -> Result<Vec<u8>> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .max_redirects(0)
        .timeout_global(Some(Duration::from_secs(120)))
        .build()
        .into();
    let mut url = trusted_url(url)?;
    for _ in 0..6 {
        let mut response = agent
            .get(url.as_str())
            .call()
            .map_err(|_| SoftwareError::RuntimeUnavailable)?;
        if response.status().is_redirection() {
            let next = response
                .headers()
                .get("location")
                .and_then(|h| h.to_str().ok())
                .ok_or(SoftwareError::RuntimeUnavailable)?;
            url = trusted_url(
                url.join(next)
                    .map_err(|_| SoftwareError::InvalidRequest)?
                    .as_str(),
            )?;
            continue;
        }
        if !response.status().is_success() {
            return Err(SoftwareError::RuntimeUnavailable);
        }
        let mut bytes = Vec::new();
        response
            .body_mut()
            .as_reader()
            .take(limit + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| SoftwareError::RuntimeUnavailable)?;
        if bytes.len() as u64 > limit {
            return Err(SoftwareError::InvalidRequest);
        }
        return Ok(bytes);
    }
    Err(SoftwareError::RuntimeUnavailable)
}
fn plain_tree(path: &Path) -> Result<()> {
    let mut next = Some(path);
    while let Some(p) = next {
        if let Ok(m) = fs::symlink_metadata(p) {
            if m.file_type().is_symlink() {
                return Err(SoftwareError::InvalidRequest);
            }
            #[cfg(windows)]
            {
                use std::os::windows::fs::MetadataExt;
                if m.file_attributes() & 0x400 != 0 {
                    return Err(SoftwareError::InvalidRequest);
                }
            }
        }
        next = p.parent();
    }
    Ok(())
}
fn safe_relative(path: &Path) -> bool {
    !path.as_os_str().is_empty() && path.components().all(|c| matches!(c, Component::Normal(_)))
}
fn extract(bytes: &[u8], stage: &Path, zip: bool) -> Result<()> {
    let bad = || SoftwareError::InvalidRequest;
    let mut total = 0u64;
    let mut seen = BTreeSet::new();
    if zip {
        let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes)).map_err(|_| bad())?;
        if archive.len() > 20000 {
            return Err(bad());
        }
        for i in 0..archive.len() {
            let mut file = archive.by_index(i).map_err(|_| bad())?;
            let relative = file.enclosed_name().ok_or_else(bad)?;
            if !safe_relative(&relative)
                || !seen.insert(relative.clone())
                || file.unix_mode().is_some_and(|m| m & 0o170000 == 0o120000)
            {
                return Err(bad());
            }
            total = total.checked_add(file.size()).ok_or_else(bad)?;
            if total > 1024 * 1024 * 1024 {
                return Err(bad());
            }
            let out = stage.join(relative);
            if file.is_dir() {
                fs::create_dir_all(out).map_err(|_| bad())?;
            } else {
                fs::create_dir_all(out.parent().ok_or_else(bad)?).map_err(|_| bad())?;
                let mut dest = fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(out)
                    .map_err(|_| bad())?;
                std::io::copy(&mut file, &mut dest).map_err(|_| bad())?;
            }
        }
    } else {
        let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(bytes));
        for (i, entry) in archive.entries().map_err(|_| bad())?.enumerate() {
            if i >= 20000 {
                return Err(bad());
            }
            let mut entry = entry.map_err(|_| bad())?;
            let path = entry.path().map_err(|_| bad())?.into_owned();
            if !safe_relative(&path) || !seen.insert(path.clone()) {
                return Err(bad());
            }
            let ty = entry.header().entry_type();
            if !ty.is_file() && !ty.is_dir() {
                return Err(bad());
            }
            total = total.checked_add(entry.size()).ok_or_else(bad)?;
            if total > 1024 * 1024 * 1024 {
                return Err(bad());
            }
            if !entry.unpack_in(stage).map_err(|_| bad())? {
                return Err(bad());
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mode = entry.header().mode().map_err(|_| bad())? & 0o755;
                fs::set_permissions(stage.join(path), fs::Permissions::from_mode(mode))
                    .map_err(|_| bad())?;
            }
        }
    }
    Ok(())
}
/// Stages without touching game files. Exact major version is published once.
pub async fn download(config: &HostConfiguration, major: u32) -> Result<JavaRuntime> {
    if !(8..=99).contains(&major) {
        return Err(SoftwareError::InvalidRequest);
    }
    let root = managed_root(config);
    plain_tree(&root)?;
    fs::create_dir_all(&root).map_err(|_| SoftwareError::RuntimeUnavailable)?;
    let final_dir = root.join(format!("temurin-{major}"));
    if final_dir.exists() {
        return find_runtime(&final_dir, major).await;
    }
    let stage = root.join(format!(".download-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&stage).map_err(|_| SoftwareError::RuntimeUnavailable)?;
    let staged = stage.clone();
    let transfer=tokio::task::spawn_blocking(move||->Result<()> {
        let os=if cfg!(windows){"windows"}else if cfg!(target_os="macos"){"mac"}else{"linux"};
        let arch=match std::env::consts::ARCH {"x86_64"=>"x64","aarch64"=>"aarch64",_=>return Err(SoftwareError::InvalidRequest)};
        let url=format!("https://api.adoptium.net/v3/assets/latest/{major}/hotspot?architecture={arch}&image_type=jre&os={os}&vendor=eclipse&release_type=ga");
        let metadata=fetch(&url,1024*1024)?;let value:serde_json::Value=serde_json::from_slice(&metadata).map_err(|_|SoftwareError::RuntimeUnavailable)?;
        let package=&value[0]["binary"]["package"];let checksum=package["checksum"].as_str().filter(|s|s.len()==64&&s.bytes().all(|b|b.is_ascii_hexdigit())).ok_or(SoftwareError::RuntimeUnavailable)?;
        let link=package["link"].as_str().ok_or(SoftwareError::RuntimeUnavailable)?;let archive=fetch(link,256*1024*1024)?;
        if hex::encode(Sha256::digest(&archive))!=checksum.to_ascii_lowercase(){return Err(SoftwareError::InvalidRequest);}
        extract(&archive,&staged,os=="windows")?;
        fs::write(staged.join("vesta-runtime-source.json"),metadata).map_err(|_|SoftwareError::RuntimeUnavailable)?;Ok(())
    }).await.map_err(|_|SoftwareError::RuntimeUnavailable)?;
    transfer?;
    let runtime = find_runtime(&stage, major).await?;
    if !matches!(
        runtime.arch.as_str(),
        "amd64" | "x86_64" | "aarch64" | "arm64"
    ) {
        return Err(SoftwareError::RuntimeUnavailable);
    }
    plain_tree(&root)?;
    fs::rename(&stage, &final_dir).map_err(|_| SoftwareError::RuntimeUnavailable)?;
    find_runtime(&final_dir, major).await
}
async fn find_runtime(root: &Path, major: u32) -> Result<JavaRuntime> {
    for path in std::iter::once(root.to_owned()).chain(
        fs::read_dir(root)
            .into_iter()
            .flatten()
            .take(16)
            .filter_map(|e| e.ok().map(|e| e.path())),
    ) {
        for java in [
            path.join("bin").join(java_leaf()),
            path.join("Contents/Home/bin").join(java_leaf()),
        ] {
            if java.is_file()
                && let Ok(runtime) = probe(&java, true).await
                && runtime.major == major
            {
                return Ok(runtime);
            }
        }
    }
    Err(SoftwareError::RuntimeUnavailable)
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    #[test]
    fn version_and_path_policies_are_closed() {
        assert_eq!(major("1.8.0_402"), Some(8));
        assert_eq!(major("25.0.1+8"), Some(25));
        assert!(!safe_relative(Path::new("../evil")));
        assert!(trusted_url("http://github.com/a").is_err());
        assert!(trusted_url("https://evil.example/a").is_err());
    }
    #[tokio::test]
    async fn real_host_java_probe_when_java_home_is_available() {
        if let Some(home) = std::env::var_os("JAVA_HOME") {
            let path = PathBuf::from(home).join("bin").join(java_leaf());
            if path.is_file() {
                let runtime = probe(&path, false).await.unwrap();
                assert!(runtime.major >= 8);
                assert!(!runtime.vendor.is_empty());
                assert!(!runtime.arch.is_empty());
            }
        }
    }
    #[tokio::test]
    #[ignore = "downloads the official checksum-verified Adoptium JRE"]
    async fn managed_adoptium_download_smoke() {
        let temp = tempfile::tempdir().unwrap();
        let doc = "schema-version=1\n[paths]\nstate='state'\ngame='game'\nlaunch-definition='launch.toml'\nscan-roots=['game/config']\n[pack]\nname='Java smoke'\nversion='1'\nrevision=1\nminimum-revision=1\nminecraft-version='1.21.4'\n[pack-access]\nmode='minecraft'\n[listener]\nmode='disabled'\n";
        let config =
            HostConfiguration::parse_toml(doc.as_bytes(), temp.path(), Path::new("host.toml"))
                .unwrap();
        let runtime = download(&config, 21).await.unwrap();
        assert_eq!(runtime.major, 21);
        assert!(runtime.managed);
        assert_eq!(download(&config, 21).await.unwrap().id, runtime.id);
    }
    #[test]
    fn archive_traversal_and_links_refuse() {
        let mut bytes = Vec::new();
        {
            let mut writer = zip::ZipWriter::new(std::io::Cursor::new(&mut bytes));
            writer
                .start_file("../escape", zip::write::SimpleFileOptions::default())
                .unwrap();
            writer.write_all(b"bad").unwrap();
            writer.finish().unwrap();
        }
        let stage = tempfile::tempdir().unwrap();
        assert!(extract(&bytes, stage.path(), true).is_err());
    }
}
