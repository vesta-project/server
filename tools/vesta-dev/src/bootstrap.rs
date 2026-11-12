//! First-run development setup. Existing installations are never overwritten.
use super::executable;
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};
use vesta_state::StateDirectory;

type Result<T> = std::result::Result<T, String>;
const PENDING: &str = ".dev-bootstrap-pending";

fn prepare(path: &Path) -> Result<StateDirectory> {
    let state = StateDirectory::new(path, None).map_err(|e| e.code().to_owned())?;
    state.prepare().map_err(|e| e.code().to_owned())?;
    Ok(state)
}
fn write_new(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|e| e.to_string())?;
    file.write_all(bytes).map_err(|e| e.to_string())
}
fn quote(path: &Path) -> Result<String> {
    serde_json::to_string(&path.to_string_lossy().replace('\\', "/")).map_err(|e| e.to_string())
}

fn java() -> Result<PathBuf> {
    let name = executable("java");
    if let Some(path) = std::env::var_os("VESTA_DEV_JAVA") {
        return fs::canonicalize(path)
            .map_err(|_| "VESTA_DEV_JAVA must name a Java executable".into());
    }
    if let Some(home) = std::env::var_os("JAVA_HOME") {
        let path = PathBuf::from(home).join("bin").join(&name);
        if path.is_file() {
            return fs::canonicalize(path).map_err(|e| e.to_string());
        }
    }
    for directory in std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()) {
        let path = directory.join(&name);
        if path.is_file() {
            return fs::canonicalize(path).map_err(|e| e.to_string());
        }
    }
    Err("First-run dev setup needs Java 21 for Minecraft 1.21.1. Set JAVA_HOME or VESTA_DEV_JAVA; existing data is unchanged".into())
}

fn has_install(root: &Path) -> Result<bool> {
    let path = root.join("names/dev-server.json");
    match fs::symlink_metadata(&path) {
        Ok(_) => {
            super::restart::guarded(&path)?;
            Ok(true)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e.to_string()),
    }
}

/// Only the implicit development registry may be bootstrapped. Custom registries
/// and named restart targets keep the normal update-only behavior.
pub fn ensure(bin: &Path, root: &Path) -> Result<bool> {
    prepare(root)?;
    let pending = root.join(PENDING);
    let installed = has_install(root)?;
    if installed && !pending.exists() {
        return Ok(false);
    }
    if pending.exists() {
        super::restart::guarded(&pending)?;
        match fs::read(&pending).map_err(|e| e.to_string())?.as_slice() {
            b"vesta-dev-configured-v1\n" if installed => return Ok(true),
            b"vesta-dev-bootstrap-v1\n" => (),
            _ => return Err("unknown dev setup marker; retained for review".into()),
        }
    }
    if !installed {
        refuse_orphaned_instances(root)?;
        let java = java()?;
        if pending.exists() {
            super::restart::guarded(&pending)?;
        } else {
            write_new(&pending, b"vesta-dev-bootstrap-v1\n")?;
        }
        let version =
            std::env::var("VESTA_DEV_MINECRAFT_VERSION").unwrap_or_else(|_| "1.21.1".into());
        println!(
            "Creating dev-server (Minecraft {version}). Data: {}",
            root.display()
        );
        println!("Minecraft stays stopped; its EULA remains unaccepted.");
        let status = Command::new(bin.join(executable("vesta")))
            .args(["create", "dev-server", "--install-root"])
            .arg(root)
            .args(["--minecraft-version", &version, "--java"])
            .arg(java)
            .args([
                "--loader",
                "vanilla",
                "--bind",
                "127.0.0.1",
                "--port",
                "25565",
                "--agent-startup",
                "manual",
                "--approve-install",
                "--ack-warnings",
                "--ack-legacy-checksum",
            ])
            .stdin(Stdio::null())
            .status()
            .map_err(|e| e.to_string())?;
        if !status.success() {
            return Err("dev-server creation failed; retained files for recovery. Fix the reported error and rerun ./dev.ps1".into());
        }
    }
    configure(root)?;
    fs::write(&pending, b"vesta-dev-configured-v1\n").map_err(|e| e.to_string())?;
    Ok(true)
}

pub fn finish(root: &Path) -> Result<()> {
    fs::remove_file(root.join(PENDING)).map_err(|e| e.to_string())
}

fn refuse_orphaned_instances(root: &Path) -> Result<()> {
    let instances = root.join("instances");
    match fs::symlink_metadata(&instances) {
        Ok(_) => {
            super::restart::guarded(&instances)?;
            if fs::read_dir(instances)
                .map_err(|e| e.to_string())?
                .next()
                .is_some()
            {
                return Err("dev-server registration is missing but instance data remains; refusing to replace it. Recover the registration or select another --server-root".into());
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
        Err(e) => return Err(e.to_string()),
    }
    Ok(())
}

fn configure(root: &Path) -> Result<()> {
    let record = root.join("names/dev-server.json");
    super::restart::guarded(&record)?;
    let record: serde_json::Value =
        serde_json::from_slice(&fs::read(record).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
    let id = record["instance"]
        .as_str()
        .filter(|id| {
            id.len() == 36
                && id.starts_with("srv_")
                && id[4..]
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        })
        .ok_or("invalid new dev-server record")?;
    let source = root.join("instances").join(id).join("host.toml");
    super::restart::guarded(&source)?;
    let private = root.join(".dev-tls");
    let state = prepare(&private)?;
    let key = private.join("key.pem");
    let cert = private.join("certificate.pem");
    if !key.exists() && !cert.exists() {
        let pair = rcgen::KeyPair::generate().map_err(|e| e.to_string())?;
        let certificate =
            rcgen::CertificateParams::new(vec!["localhost".into(), "127.0.0.1".into()])
                .and_then(|params| params.self_signed(&pair))
                .map_err(|e| e.to_string())?;
        write_new(&key, pair.serialize_pem().as_bytes())?;
        state.tighten_file(&key).map_err(|e| e.code().to_owned())?;
        write_new(&cert, certificate.pem().as_bytes())?;
        state.tighten_file(&cert).map_err(|e| e.code().to_owned())?;
    }
    super::restart::guarded(&key)?;
    super::restart::guarded(&cert)?;
    rcgen::KeyPair::from_pem(&fs::read_to_string(&key).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    let listener = format!(
        "[listener]\nmode = 'tls'\nbind = '127.0.0.1'\nport = 8443\npublic-endpoint = 'https://127.0.0.1:8443/'\ncertificate = {}\nprivate-key = {}\nmax-connections = 32\nmax-request-bytes = 1048576\nrequest-timeout-seconds = 30\n",
        quote(&cert)?,
        quote(&key)?
    );
    let document = fs::read_to_string(&source)
        .map_err(|e| e.to_string())?
        .replace("\r\n", "\n");
    if let Some(prefix) = document.strip_suffix("[listener]\nmode = 'disabled'\n") {
        let temporary = source.with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
        write_new(&temporary, format!("{prefix}{listener}").as_bytes())?;
        fs::rename(temporary, &source).map_err(|e| e.to_string())?;
    } else if !document.ends_with(&listener) {
        return Err(
            "new dev-server configuration changed during setup; retained for review".into(),
        );
    }
    println!("Dev management endpoint: https://127.0.0.1:8443/");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn existing_registration_is_never_reinstalled_and_orphaned_data_is_preserved() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("servers");
        fs::create_dir_all(root.join("instances/keep/game")).unwrap();
        fs::write(root.join("instances/keep/game/world"), b"world").unwrap();
        assert!(refuse_orphaned_instances(&root).is_err());
        fs::create_dir_all(root.join("names")).unwrap();
        fs::write(root.join("names/dev-server.json"), b"existing registration").unwrap();
        assert!(!ensure(temp.path(), &root).unwrap());
        assert_eq!(
            fs::read(root.join("instances/keep/game/world")).unwrap(),
            b"world"
        );
    }
    #[test]
    fn new_configuration_and_tls_are_reused_without_changing_eula() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("servers");
        let id = "srv_0123456789abcdef0123456789abcdef";
        let directory = root.join("instances").join(id);
        fs::create_dir_all(directory.join("game/config")).unwrap();
        fs::create_dir_all(root.join("names")).unwrap();
        fs::write(
            root.join("names/dev-server.json"),
            format!("{{\"schema\":1,\"instance\":\"{id}\"}}"),
        )
        .unwrap();
        fs::write(
            directory.join("host.toml"),
            format!("schema-version=1\n[paths]\nstate={}\ngame={}\nscan-roots=[{}]\nlaunch-definition={}\n[pack]\nname='dev-server'\nversion='1'\nrevision=1\nminimum-revision=1\nminecraft-version='1.21.1'\n[pack-access]\nmode='minecraft'\n[listener]\nmode = 'disabled'\n", quote(&directory.join("state")).unwrap(), quote(&directory.join("game")).unwrap(), quote(&directory.join("game/config")).unwrap(), quote(&directory.join("launch.toml")).unwrap()),
        )
        .unwrap();
        fs::write(directory.join("game/eula.txt"), b"eula=false\n").unwrap();
        configure(&root).unwrap();
        vesta_control::config::HostConfiguration::load_file(
            &directory.join("host.toml"),
            &directory,
        )
        .unwrap();
        let key = fs::read(root.join(".dev-tls/key.pem")).unwrap();
        let document = fs::read(directory.join("host.toml")).unwrap();
        configure(&root).unwrap();
        assert_eq!(fs::read(root.join(".dev-tls/key.pem")).unwrap(), key);
        assert_eq!(fs::read(directory.join("host.toml")).unwrap(), document);
        assert_eq!(
            fs::read(directory.join("game/eula.txt")).unwrap(),
            b"eula=false\n"
        );
        // A failed first startup can be retried after correcting configuration
        // (for example, an occupied port) without rewriting that correction.
        fs::write(root.join(PENDING), b"vesta-dev-configured-v1\n").unwrap();
        fs::write(directory.join("host.toml"), b"user-corrected configuration").unwrap();
        assert!(ensure(temp.path(), &root).unwrap());
        assert_eq!(
            fs::read(directory.join("host.toml")).unwrap(),
            b"user-corrected configuration"
        );
        finish(&root).unwrap();
        assert!(!root.join(PENDING).exists());
    }
}
