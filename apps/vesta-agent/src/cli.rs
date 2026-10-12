//! Strict, non-interactive OS-local command grammar. No shell/remote options.
use std::{
    ffi::OsString,
    net::{IpAddr, Ipv4Addr},
    path::PathBuf,
};
use vesta_server_install::Software;

pub const USAGE: &str = "Usage: vesta-agent version | update | check-config <path> | status <path> | serve <path> [--local-only]\n       vesta-agent create <name> --install-root <root> --minecraft-version <version> --java <path> [--loader <vanilla|paper|folia|purpur|fabric|quilt>] [--loader-version <version>] [--bind <loopback-ip>] [--port <port>] [--approve-install] [--ack-warnings] [--ack-legacy-checksum]\n       vesta-agent delete <name> --confirm <name> [--install-root <root>]\n       vesta-agent <config|start|stop|restart> <name> [--install-root <root>]
       vesta-agent access show|set <name> [--anonymous-role role] [--new-user-role role] [--whitelist-role role] [--whitelist-sync on|off] [--password-role role] [--install-root path]\n  vesta-agent pair <name> [--install-root <root>] [--role owner|admin|administrator|maintainer|moderator|viewer|pack-user] [--expires <1..60 minutes>]
       vesta-agent devices list <name> [--install-root <root>]
       vesta-agent devices revoke <device-id> <name> [--install-root <root>]\n       vesta-agent owner link <name> --minecraft-uuid <uuid> [--role <builtin>] [--install-root <root>]\n       vesta-agent owner unlink <name> --minecraft-uuid <uuid> [--revoke-devices] [--install-root <root>]\n       vesta-agent owner list <name> [--install-root <root>]
       vesta-agent superadmin grant|revoke <user-id> <name> [--install-root <root>]
       vesta-agent resolve-launch <name> --confirm <name> [--install-root <root>]
       vesta-agent reset-restarts <name> [--install-root <root>]
       vesta-agent backup-init <name> [--install-root <root>]
       vesta-agent recovery-export <name> --output <file> [--install-root <root>]
       vesta-agent backup-resolve <name> [<job-id> --confirm <job-id>] [--install-root <root>]";
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
    Management {
        name: String,
        root: Option<PathBuf>,
        op: crate::control::Op,
    },
    Delete {
        name: String,
        confirm: String,
        root: Option<PathBuf>,
    },
    ResolveLaunch {
        name: String,
        confirm: String,
        root: Option<PathBuf>,
    },
    ResetRestarts {
        name: String,
        root: Option<PathBuf>,
    },
    BackupInit {
        name: String,
        root: Option<PathBuf>,
    },
    RecoveryExport {
        name: String,
        output: PathBuf,
        root: Option<PathBuf>,
    },
    BackupResolve {
        name: String,
        job: Option<String>,
        confirm: Option<String>,
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
        "superadmin" => return parse_superadmin(&args),
        "owner" => return parse_owner(&args),
        "access" => return parse_access(&args),
        "pair" | "devices" => return parse_management(&args),
        "recovery-export" | "backup-resolve" => return parse_recovery(&args),
        "create" | "delete" | "resolve-launch" | "reset-restarts" | "backup-init" | "config"
        | "start" | "stop" | "restart" => {}
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
            "--confirm" if command == "delete" || command == "resolve-launch" => {
                confirm = Some(text(value(&args, &mut at)?)?)
            }
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
        "resolve-launch" => Command::ResolveLaunch {
            name,
            confirm: confirm.ok_or(())?,
            root,
        },
        "reset-restarts" => Command::ResetRestarts { name, root },
        "backup-init" => Command::BackupInit { name, root },
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

/// Alias `admin` maps to the existing canonical `administrator` built-in.
pub fn pair_role(role: &str) -> Option<vesta_control::identity::BuiltinRole> {
    use vesta_control::identity::BuiltinRole;
    let role = if role == "admin" {
        "administrator"
    } else {
        role
    };
    BuiltinRole::ALL.iter().copied().find(|r| r.id() == role)
}
fn parse_management(args: &[OsString]) -> Result<Command, ()> {
    use crate::control::Op;
    let mut role = "owner".to_owned();
    let mut minutes = vesta_control::identity::management::DEFAULT_PAIR_MINUTES;
    let mut root = None;
    let pairing = args[0] == "pair";
    let (name_at, mut at, device) = if pairing {
        (1, 2, None)
    } else {
        match args.get(1).and_then(|v| v.to_str()) {
            Some("list") => (2, 3, None),
            Some("revoke") => (
                3,
                4,
                Some(args.get(2).and_then(|v| v.to_str()).ok_or(())?.to_owned()),
            ),
            _ => return Err(()),
        }
    };
    let name = args
        .get(name_at)
        .and_then(|v| v.to_str())
        .ok_or(())?
        .to_owned();
    if !valid_name(&name) {
        return Err(());
    }
    if let Some(device) = &device {
        vesta_control::identity::DeviceId::parse(device).map_err(|_| ())?;
    }
    let mut seen = std::collections::HashSet::new();
    while at < args.len() {
        let flag = args[at].to_str().ok_or(())?;
        if !seen.insert(flag) {
            return Err(());
        }
        match flag {
            "--install-root" => root = Some(PathBuf::from(value(args, &mut at)?)),
            "--role" if pairing => {
                role = text(value(args, &mut at)?)?;
                pair_role(&role).ok_or(())?;
            }
            "--expires" if pairing => {
                minutes = text(value(args, &mut at)?)?.parse().map_err(|_| ())?;
                if !(1..=vesta_control::identity::management::MAX_PAIR_MINUTES).contains(&minutes) {
                    return Err(());
                }
            }
            _ => return Err(()),
        }
        at += 1;
    }
    let op = if pairing {
        Op::Pair { role, minutes }
    } else if let Some(device) = device {
        Op::DevicesRevoke { device }
    } else {
        Op::DevicesList
    };
    Ok(Command::Management { name, root, op })
}

fn parse_owner(args: &[OsString]) -> Result<Command, ()> {
    use crate::control::Op;
    let action = args.get(1).and_then(|v| v.to_str()).ok_or(())?;
    if !matches!(action, "link" | "unlink" | "list") {
        return Err(());
    }
    let name = args.get(2).and_then(|v| v.to_str()).ok_or(())?.to_owned();
    if !valid_name(&name) {
        return Err(());
    }
    let (mut root, mut uuid) = (None, None);
    let mut role = "owner".to_owned();
    let mut revoke_devices = false;
    let mut seen = std::collections::HashSet::new();
    let mut at = 3;
    while at < args.len() {
        let flag = args[at].to_str().ok_or(())?;
        if !seen.insert(flag) {
            return Err(());
        }
        match flag {
            "--install-root" => root = Some(PathBuf::from(value(args, &mut at)?)),
            "--minecraft-uuid" if action != "list" => {
                let v = text(value(args, &mut at)?)?;
                vesta_control::identity::minecraft::MinecraftUuid::parse(&v).map_err(|_| ())?;
                uuid = Some(v);
            }
            "--role" if action == "link" => {
                role = text(value(args, &mut at)?)?;
                pair_role(&role).ok_or(())?;
            }
            "--revoke-devices" if action == "unlink" => revoke_devices = true,
            _ => return Err(()),
        }
        at += 1;
    }
    let op = match action {
        "link" => Op::OwnerLink {
            uuid: uuid.ok_or(())?,
            role,
        },
        "unlink" => Op::OwnerUnlink {
            uuid: uuid.ok_or(())?,
            revoke_devices,
        },
        _ => Op::OwnerList,
    };
    Ok(Command::Management { name, root, op })
}

#[cfg(test)]
mod management_grammar_tests {
    use super::*;
    fn p(args: &[&str]) -> Result<Command, ()> {
        parse(args.iter().map(OsString::from))
    }
    #[test]
    fn pair_devices_and_owner_links_have_strict_bounded_grammar() {
        for args in [
            vec!["pair", "owned"],
            vec!["pair", "owned", "--role", "admin", "--expires", "60"],
            vec!["devices", "list", "owned"],
            vec!["devices", "revoke", "device-id", "owned"],
            vec![
                "owner",
                "link",
                "owned",
                "--minecraft-uuid",
                "11111111-2222-4333-8444-555555555555",
            ],
            vec![
                "owner",
                "unlink",
                "owned",
                "--minecraft-uuid",
                "11111111-2222-4333-8444-555555555555",
                "--revoke-devices",
            ],
            vec!["owner", "list", "owned"],
        ] {
            assert!(p(&args).is_ok(), "{args:?}");
        }
        for role in vesta_control::identity::BuiltinRole::ALL {
            assert!(p(&["pair", "owned", "--role", role.id()]).is_ok());
        }
        for args in [
            vec!["pair", "owned", "--expires", "0"],
            vec!["pair", "owned", "--expires", "61"],
            vec!["pair", "owned", "--expires", "-1"],
            vec!["pair", "owned", "--role", "root"],
            vec!["pair", "owned", "--role", "owner", "--role", "admin"],
            vec!["pair", "owned", "--password", "pack"],
            vec!["devices", "list"],
            vec!["devices", "revoke", "../id", "owned"],
            vec!["owner", "link", "owned"],
            vec!["owner", "link", "owned", "--minecraft-uuid", "bad"],
            vec![
                "owner",
                "unlink",
                "owned",
                "--minecraft-uuid",
                "11111111-2222-4333-8444-555555555555",
                "--role",
                "owner",
            ],
            vec![
                "owner",
                "list",
                "owned",
                "--minecraft-uuid",
                "11111111-2222-4333-8444-555555555555",
            ],
        ] {
            assert!(p(&args).is_err(), "{args:?}");
        }
    }
}

fn parse_access(args: &[OsString]) -> Result<Command, ()> {
    use crate::control::Op;
    let action = args.get(1).and_then(|a| a.to_str()).ok_or(())?;
    if !matches!(action, "show" | "set") {
        return Err(());
    }
    let name = args.get(2).and_then(|a| a.to_str()).ok_or(())?.to_owned();
    if !valid_name(&name) {
        return Err(());
    }
    let mut root = None;
    let mut fields = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut at = 3;
    while at < args.len() {
        let flag = args[at].to_str().ok_or(())?;
        if !seen.insert(flag) {
            return Err(());
        }
        let v = text(value(args, &mut at)?)?;
        if flag == "--install-root" {
            root = Some(PathBuf::from(v));
        } else if action == "set" {
            let key = match flag {
                "--anonymous-role" => "anonymous",
                "--new-user-role" => "new_user",
                "--whitelist-role" => "whitelist",
                "--password-role" => "password",
                "--whitelist-sync" => "whitelist_enabled",
                _ => return Err(()),
            };
            if key == "whitelist_enabled" {
                if !matches!(v.as_str(), "on" | "off") {
                    return Err(());
                }
            } else {
                vesta_control::identity::RoleId::parse(&v).map_err(|_| ())?;
            }
            fields.push((key.into(), v));
        } else {
            return Err(());
        }
        at += 1;
    }
    if action == "set" && fields.is_empty() {
        return Err(());
    }
    let op = if action == "show" {
        Op::AccessShow
    } else {
        Op::AccessSet { fields }
    };
    Ok(Command::Management { name, root, op })
}

#[cfg(test)]
mod access_grammar_tests {
    use super::*;
    fn p(a: &[&str]) -> Result<Command, ()> {
        parse(a.iter().map(OsString::from))
    }
    #[test]
    fn strict_access_source_flags() {
        assert!(p(&["access", "show", "owned"]).is_ok());
        assert!(
            p(&[
                "access",
                "set",
                "owned",
                "--anonymous-role",
                "player",
                "--new-user-role",
                "public",
                "--whitelist-sync",
                "off",
                "--password-role",
                "player"
            ])
            .is_ok()
        );
        for args in [
            vec!["access", "set", "owned"],
            vec!["access", "show", "owned", "--anonymous-role", "player"],
            vec!["access", "set", "owned", "--whitelist-sync", "yes"],
            vec![
                "access",
                "set",
                "owned",
                "--anonymous-role",
                "player",
                "--anonymous-role",
                "public",
            ],
            vec!["access", "set", "owned", "--password", "secret"],
            vec!["access", "edit", "owned"],
        ] {
            assert!(p(&args).is_err(), "{args:?}");
        }
    }
}

fn parse_superadmin(args: &[OsString]) -> Result<Command, ()> {
    let grant = match args.get(1).and_then(|v| v.to_str()) {
        Some("grant") => true,
        Some("revoke") => false,
        _ => return Err(()),
    };
    let user = args.get(2).and_then(|v| v.to_str()).ok_or(())?.to_owned();
    vesta_control::identity::PrincipalId::parse(&user).map_err(|_| ())?;
    let name = args.get(3).and_then(|v| v.to_str()).ok_or(())?.to_owned();
    if !valid_name(&name) {
        return Err(());
    }
    let root = match args.len() {
        4 => None,
        6 if args[4] == "--install-root" && !args[5].is_empty() => Some(PathBuf::from(&args[5])),
        _ => return Err(()),
    };
    Ok(Command::Management {
        name,
        root,
        op: crate::control::Op::Superadmin { user, grant },
    })
}

fn parse_recovery(args: &[OsString]) -> Result<Command, ()> {
    let export = args[0] == "recovery-export";
    let name = args.get(1).and_then(|v| v.to_str()).ok_or(())?.to_owned();
    if !valid_name(&name) {
        return Err(());
    }
    let (mut root, mut output, mut confirm, mut job) = (None, None, None, None);
    let mut seen = std::collections::HashSet::new();
    let mut at = 2;
    if !export
        && args
            .get(at)
            .is_some_and(|v| !v.to_string_lossy().starts_with('-'))
    {
        let id = args[at].to_str().ok_or(())?;
        if id.is_empty()
            || id.len() > 128
            || !id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
        {
            return Err(());
        }
        job = Some(id.to_owned());
        at += 1;
    }
    while at < args.len() {
        let flag = args[at].to_str().ok_or(())?;
        if !seen.insert(flag) {
            return Err(());
        }
        match flag {
            "--install-root" => root = Some(PathBuf::from(value(args, &mut at)?)),
            "--output" if export => output = Some(PathBuf::from(value(args, &mut at)?)),
            "--confirm" if !export => confirm = Some(text(value(args, &mut at)?)?),
            _ => return Err(()),
        }
        at += 1;
    }
    if export {
        Ok(Command::RecoveryExport {
            name,
            output: output.ok_or(())?,
            root,
        })
    } else {
        if job.is_some() != confirm.is_some() {
            return Err(());
        }
        Ok(Command::BackupResolve {
            name,
            job,
            confirm,
            root,
        })
    }
}

#[cfg(test)]
mod recovery_grammar_tests {
    use super::*;
    fn p(args: &[&str]) -> Result<Command, ()> {
        parse(args.iter().map(OsString::from))
    }

    #[test]
    fn recovery_commands_have_strict_named_grammar() {
        assert!(matches!(
            p(&["recovery-export", "owned", "--output", "recovery.json"]),
            Ok(Command::RecoveryExport { .. })
        ));
        assert!(
            p(&[
                "recovery-export",
                "owned",
                "--install-root",
                "root",
                "--output",
                "recovery.json"
            ])
            .is_ok()
        );
        assert!(matches!(
            p(&["backup-resolve", "owned"]),
            Ok(Command::BackupResolve {
                job: None,
                confirm: None,
                ..
            })
        ));
        assert!(
            p(&[
                "backup-resolve",
                "owned",
                "job-1",
                "--confirm",
                "job-1",
                "--install-root",
                "root"
            ])
            .is_ok()
        );
        // Confirmation equality is enforced by execution, just like resolve-launch.
        assert!(p(&["backup-resolve", "owned", "job-1", "--confirm", "job-2"]).is_ok());
        for args in [
            vec!["recovery-export"],
            vec!["recovery-export", "owned"],
            vec!["recovery-export", "../owned", "--output", "out"],
            vec![
                "recovery-export",
                "owned",
                "--output",
                "out",
                "--output",
                "other",
            ],
            vec![
                "recovery-export",
                "owned",
                "--output",
                "out",
                "--confirm",
                "owned",
            ],
            vec!["recovery-export", "owned", "--output", ""],
            vec!["backup-resolve"],
            vec!["backup-resolve", "owned", "job-1"],
            vec!["backup-resolve", "owned", "--confirm", "job-1"],
            vec!["backup-resolve", "owned", "../job", "--confirm", "../job"],
            vec![
                "backup-resolve",
                "owned",
                "job",
                "--confirm",
                "job",
                "extra",
            ],
            vec![
                "backup-resolve",
                "owned",
                "job",
                "--confirm",
                "job",
                "--confirm",
                "job",
            ],
            vec!["backup-resolve", "owned", "--output", "out"],
            vec![
                "backup-resolve",
                "owned",
                "--install-root",
                "root",
                "--install-root",
                "other",
            ],
        ] {
            assert!(p(&args).is_err(), "{args:?}");
        }
    }
}
