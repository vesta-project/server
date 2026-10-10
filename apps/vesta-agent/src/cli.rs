//! Strict, non-interactive OS-local command grammar. No shell/remote options.
use std::{
    ffi::OsString,
    net::{IpAddr, Ipv4Addr},
    path::PathBuf,
};
use vesta_server_install::Software;

pub const USAGE: &str = "Usage: vesta-agent version | update | check-config <path> | status <path> | serve <path> [--local-only]\n       vesta-agent create <name> --install-root <root> --minecraft-version <version> --java <path> [--loader <vanilla|paper|folia|purpur|fabric|quilt>] [--loader-version <version>] [--bind <loopback-ip>] [--port <port>] [--approve-install] [--ack-warnings] [--ack-legacy-checksum]\n       vesta-agent delete <name> --confirm <name> [--install-root <root>]\n       vesta-agent <config|start|stop|restart> <name> [--install-root <root>]";
#[derive(Debug)]
pub enum Command {
    Help,
    Version,
    Update,
    Check(PathBuf),
    Status(PathBuf),
    Serve {
        source: PathBuf,
        local_only: bool,
    },
    Create(Create),
    Delete {
        name: String,
        confirm: String,
        root: Option<PathBuf>,
    },
    Named {
        op: NamedOp,
        name: String,
        root: Option<PathBuf>,
    },
}
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum NamedOp {
    Config,
    Start,
    Stop,
    Restart,
}
#[derive(Debug)]
pub struct Create {
    pub name: String,
    pub root: PathBuf,
    pub version: String,
    pub java: PathBuf,
    pub software: Software,
    pub loader_version: Option<String>,
    pub bind: IpAddr,
    pub port: u16,
    pub approve: bool,
    pub warnings: bool,
    pub legacy: bool,
}
pub fn valid_name(name: &str) -> bool {
    !matches!(
        name,
        "con"
            | "prn"
            | "aux"
            | "nul"
            | "com1"
            | "com2"
            | "com3"
            | "com4"
            | "com5"
            | "com6"
            | "com7"
            | "com8"
            | "com9"
            | "lpt1"
            | "lpt2"
            | "lpt3"
            | "lpt4"
            | "lpt5"
            | "lpt6"
            | "lpt7"
            | "lpt8"
            | "lpt9"
    ) && !name.is_empty()
        && name.len() <= 48
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && name.as_bytes()[0].is_ascii_alphanumeric()
}
fn value(args: &[OsString], at: &mut usize) -> Result<OsString, ()> {
    *at += 1;
    let v = args.get(*at).ok_or(())?;
    if v.is_empty() || v.to_string_lossy().starts_with('-') {
        return Err(());
    }
    Ok(v.clone())
}
fn text(v: OsString) -> Result<String, ()> {
    v.into_string().map_err(|_| ())
}
pub fn parse(args: impl IntoIterator<Item = OsString>) -> Result<Command, ()> {
    let args: Vec<_> = args.into_iter().collect();
    let command = args.first().and_then(|s| s.to_str()).ok_or(())?;
    match command {
        "--help" | "-h" if args.len() == 1 => return Ok(Command::Help),
        "version" if args.len() == 1 => return Ok(Command::Version),
        "update" if args.len() == 1 => return Ok(Command::Update),
        "status" | "check-config" | "serve" => {
            let mut at = 0;
            let source = PathBuf::from(value(&args, &mut at)?);
            let local_only = command == "serve" && args.len() == 3 && args[2] == "--local-only";
            if args.len() != 2 && !local_only {
                return Err(());
            }
            return Ok(match command {
                "status" => Command::Status(source),
                "check-config" => Command::Check(source),
                _ => Command::Serve { source, local_only },
            });
        }
        "create" | "delete" | "config" | "start" | "stop" | "restart" => {}
        _ => return Err(()),
    }
    let name = args.get(1).and_then(|v| v.to_str()).ok_or(())?.to_owned();
    if !valid_name(&name) {
        return Err(());
    }
    let (mut root, mut version, mut java, mut loader_version, mut confirm) =
        (None, None, None, None, None);
    let mut software = Software::Vanilla;
    let mut bind = IpAddr::V4(Ipv4Addr::LOCALHOST);
    let mut port = 25565;
    let (mut approve, mut warnings, mut legacy) = (false, false, false);
    let mut seen = std::collections::HashSet::new();
    let mut at = 2;
    while at < args.len() {
        let flag = args[at].to_str().ok_or(())?;
        if !seen.insert(flag) {
            return Err(());
        }
        match flag {
            "--install-root" => root = Some(PathBuf::from(value(&args, &mut at)?)),
            "--confirm" if command == "delete" => confirm = Some(text(value(&args, &mut at)?)?),
            "--minecraft-version" if command == "create" => {
                version = Some(text(value(&args, &mut at)?)?)
            }
            "--java" if command == "create" => java = Some(PathBuf::from(value(&args, &mut at)?)),
            "--loader-version" if command == "create" => {
                loader_version = Some(text(value(&args, &mut at)?)?)
            }
            "--loader" if command == "create" => {
                software = match text(value(&args, &mut at)?)?.as_str() {
                    "vanilla" => Software::Vanilla,
                    "paper" => Software::Paper,
                    "folia" => Software::Folia,
                    "purpur" => Software::Purpur,
                    "fabric" => Software::Fabric,
                    "quilt" => Software::Quilt,
                    _ => return Err(()),
                }
            }
            "--bind" if command == "create" => {
                bind = text(value(&args, &mut at)?)?.parse().map_err(|_| ())?
            }
            "--port" if command == "create" => {
                port = text(value(&args, &mut at)?)?.parse().map_err(|_| ())?
            }
            "--approve-install" if command == "create" => approve = true,
            "--ack-warnings" if command == "create" => warnings = true,
            "--ack-legacy-checksum" if command == "create" => legacy = true,
            _ => return Err(()),
        }
        at += 1;
    }
    Ok(match command {
        "create" => {
            let version: String = version.ok_or(())?;
            if version.len() > 64
                || !version
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._-+".contains(&b))
                || port == 0
                || !bind.is_loopback()
                || (loader_version.is_some()
                    && !matches!(software, Software::Fabric | Software::Quilt))
            {
                return Err(());
            }
            Command::Create(Create {
                name,
                root: root.ok_or(())?,
                version,
                java: java.ok_or(())?,
                software,
                loader_version,
                bind,
                port,
                approve,
                warnings,
                legacy,
            })
        }
        "delete" => Command::Delete {
            name,
            confirm: confirm.ok_or(())?,
            root,
        },
        _ => Command::Named {
            op: match command {
                "config" => NamedOp::Config,
                "start" => NamedOp::Start,
                "stop" => NamedOp::Stop,
                _ => NamedOp::Restart,
            },
            name,
            root,
        },
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    fn p(args: &[&str]) -> Result<Command, ()> {
        parse(args.iter().map(OsString::from))
    }
    #[test]
    fn strict_grammar() {
        for args in [
            vec![],
            vec!["version", "extra"],
            vec!["update", "--force"],
            vec!["stop", "s", "--force"],
            vec!["create", "s"],
            vec!["delete", "s"],
            vec!["status", "--secret"],
            vec!["serve", "x", "--remote"],
            vec!["config", "../s"],
            vec!["start", "s", "--install-root", "x", "--install-root", "y"],
        ] {
            assert!(p(&args).is_err(), "{args:?}");
        }
        assert!(p(&["delete", "s", "--confirm", "s"]).is_ok());
        assert!(p(&["serve", "x", "--local-only"]).is_ok());
        for command in ["version", "update", "--help"] {
            assert!(p(&[command]).is_ok());
        }
    }
    #[test]
    fn create_flags_never_accept_eula_or_public_bind() {
        let base = [
            "create",
            "owned",
            "--install-root",
            "root",
            "--minecraft-version",
            "1.21.1",
            "--java",
            "java",
        ];
        assert!(p(&base).is_ok());
        for flags in [
            ["--accept-eula", "yes"],
            ["--bind", "0.0.0.0"],
            ["--loader", "forge"],
            ["--port", "0"],
        ] {
            let mut args = base.to_vec();
            args.extend(flags);
            assert!(p(&args).is_err());
        }
    }
}
