//! Local development fixture support, invoked only by scripts/dev-test-server.*.
//! Reuses setup's TLS generator; never installed as a production agent command.
use rustls_pki_types::{CertificateDer, pem::PemObject};
use sha2::{Digest, Sha256};
use std::{fs, io::Write, path::Path};

fn write_new(path: &Path, bytes: &[u8]) -> Result<(), Box<dyn std::error::Error>> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)?.write_all(bytes)?;
    Ok(())
}
fn quote(path: &Path) -> Result<String, Box<dyn std::error::Error>> {
    Ok(serde_json::to_string(
        &path.to_string_lossy().replace('\\', "/"),
    )?)
}
fn run() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    let mode = args
        .first()
        .and_then(|s| s.to_str())
        .ok_or("missing mode")?;
    let root = Path::new(args.get(1).ok_or("missing root")?);
    if !root.is_absolute() {
        return Err("root must be absolute".into());
    }
    vesta_state::StateDirectory::new(root, None)?.prepare()?;
    match mode {
        "tls" if args.len() == 2 => {
            let key_path = root.join("key.pem");
            let cert_path = root.join("certificate.pem");
            if !key_path.exists() && !cert_path.exists() {
                let key = rcgen::KeyPair::generate()?;
                let cert = rcgen::CertificateParams::new(vec!["localhost".into(), "127.0.0.1".into()])?.self_signed(&key)?;
                write_new(&key_path,key.serialize_pem().as_bytes())?;
                write_new(&cert_path,cert.pem().as_bytes())?;
            }
            // Fail closed on interrupted/invalid material; never silently rotate trust.
            rcgen::KeyPair::from_pem(&fs::read_to_string(key_path)?)?;
            let cert = CertificateDer::from_pem_slice(&fs::read(cert_path)?)?;
            println!("TLS SHA-256 fingerprint: {:x}",Sha256::digest(cert.as_ref()));
        }
        "inert" if args.len() == 3 => {
            let id = format!("srv_{}",uuid::Uuid::new_v4().simple());
            let directory = root.join("instances").join(&id);
            let game = directory.join("game");
            fs::create_dir_all(game.join("config"))?;
            fs::create_dir_all(root.join("names"))?;
            fs::create_dir_all(root.join("recoverable"))?;
            let child = directory.join(if cfg!(windows) { "inert.exe" } else { "inert" });
            fs::copy(Path::new(&args[2]),&child)?;
            write_new(&game.join("tls-inert-enabled"),b"vesta-owned-tls-inert-v1")?;
            write_new(&game.join("eula.txt"),b"# Inert test fixture only: no Minecraft software or EULA acceptance.\neula=true\n")?;
            let launch = directory.join("launch.toml");
            write_new(&launch,format!("[minecraft]\nworking-directory = {}\n[launch]\nkind = 'native'\nexecutable = {}\nargs = ['--exact','management_inert_child','--nocapture','--test-threads=1']\nstop = 'console-stop'\n",quote(&game)?,quote(&child)?).as_bytes())?;
            write_new(&directory.join("host.toml"),format!("schema-version = 1\n[paths]\nstate = {}\ngame = {}\nscan-roots = [{}]\nlaunch-definition = {}\n[pack]\nname = 'Vesta dev inert fixture'\nversion = '1.0'\nrevision = 1\nminimum-revision = 1\nminecraft-version = 'inert'\n[pack-access]\nmode = 'minecraft'\n[listener]\nmode = 'disabled'\n",quote(&directory.join("state"))?,quote(&game)?,quote(&game.join("config"))?,quote(&launch)?).as_bytes())?;
            write_new(&directory.join("local-name"),b"dev-server")?;
            write_new(&root.join("names/dev-server.json"),&serde_json::to_vec(&serde_json::json!({"schema":1,"instance":id}))?)?;
            println!("Inert integration-test child installed; no Minecraft software downloaded.");
        }
        _ => return Err("usage: dev-test-support tls <private-dir> | inert <install-root> <https-test-executable>".into()),
    }
    Ok(())
}
fn main() -> std::process::ExitCode {
    match run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("Development fixture setup failed: {e}");
            std::process::ExitCode::FAILURE
        }
    }
}
