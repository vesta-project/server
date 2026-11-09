//! Strict OS-local command grammar. Scoped management never executes an OS shell.
use std::{
    ffi::OsString,
    net::{IpAddr, Ipv4Addr},
    path::PathBuf,
};
use vesta_server_install::Software;

pub const USAGE: &str = "Vesta agent\n\n  vesta list [--json] [--install-root <root>]\n  vesta manage <server_id|name> [command ...] [--install-root <root>]\n  vesta manage --help\n  vesta create <name> --install-root <root> --minecraft-version <version> --java <path> [--agent-startup manual|boot] [install flags]\n  vesta agent serve <host.toml> [--local-only|--allow-local]\n  vesta agent check-config|status <host.toml>\n  vesta version | update\n\nUse manage without a command to enter the server terminal.\nThe vesta-agent executable supports the same commands.";
#[derive(Debug)]
pub enum Command {
    Help,
    CreateHelp,
    ManageHelp,
    UninstallHelp,
    Uninstall(crate::uninstall::Request),
    UninstallExecute {
        job: PathBuf,
        token: String,
    },
    Shutdown(PathBuf),
    List {
        root: Option<PathBuf>,
        json: bool,
    },
    Manage {
        target: String,
        root: Option<PathBuf>,
        args: Vec<OsString>,
    },
    Version,
    Update(Vec<String>),
    Check(PathBuf),
    Status(PathBuf),
    Serve {
        source: PathBuf,
        local_only: bool,
        allow_local: bool,
    },
    AgentStartup {
        name: String,
        root: PathBuf,
        choice: Option<AgentStartupChoice>,
    },
    AgentStart {
        name: String,
        root: PathBuf,
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
        passphrase_file: Option<PathBuf>,
        root: Option<PathBuf>,
    },
    RecoveryImport {
        name: String,
        input: PathBuf,
        passphrase_file: Option<PathBuf>,
        confirm: String,
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
    pub agent_startup: Option<AgentStartupChoice>,
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentStartupChoice {
    Manual,
    Boot,
}
impl AgentStartupChoice {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "manual" => Some(Self::Manual),
            "boot" => Some(Self::Boot),
            _ => None,
        }
    }
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
        "uninstall"
            if args.len() == 2 && matches!(args[1].to_str(), Some("--help" | "-h" | "help")) =>
        {
            return Ok(Command::UninstallHelp);
        }
        "uninstall" => return crate::uninstall::parse(&args).map(Command::Uninstall),
        "uninstall-execute" if args.len() == 3 => {
            return Ok(Command::UninstallExecute {
                job: PathBuf::from(&args[1]),
                token: args[2].to_str().ok_or(())?.to_owned(),
            });
        }
        "create"
            if args.len() == 2 && matches!(args[1].to_str(), Some("--help" | "-h" | "help")) =>
        {
            return Ok(Command::CreateHelp);
        }
        "--help" | "-h" | "help" if args.len() == 1 => return Ok(Command::Help),
        "list" | "servers" => return crate::manage::parse_list(&args),
        "manage" => return crate::manage::parse(&args),
        "agent" => {
            if args.len() == 2 && matches!(args[1].to_str(), Some("help" | "--help" | "-h")) {
                return Ok(Command::Help);
            }
            if !matches!(
                args.get(1).and_then(|s| s.to_str()),
                Some("serve" | "status" | "check-config" | "shutdown" | "version" | "update")
            ) {
                return Err(());
            }
            return parse(args.into_iter().skip(1));
        }
        "version" if args.len() == 1 => return Ok(Command::Version),
        "update" => {
            if args.len() > 1
                && !args[1].to_str().is_some_and(vesta_setup::updates::accepts_action)
            {
                return Err(());
            }
            return Ok(Command::Update(
                args.iter().skip(1)
                    .map(|a| a.to_str().map(str::to_owned).ok_or(()))
                    .collect::<Result<Vec<_>, _>>()?,
            ));
        }
        "status" | "check-config" | "serve" | "shutdown" => {
            let mut at = 0;
            let source = PathBuf::from(value(&args, &mut at)?);
            let local_only = command == "serve" && args.len() == 3 && args[2] == "--local-only";
            let allow_local = command == "serve" && args.len() == 3 && args[2] == "--allow-local";
            if args.len() != 2 && !local_only && !allow_local {
                return Err(());
            }
            return Ok(match command {
                "status" => Command::Status(source),
                "shutdown" => Command::Shutdown(source),
                "check-config" => Command::Check(source),
                _ => Command::Serve {
                    source,
                    local_only,
                    allow_local,
                },
            });
        }
        "superadmin" => return parse_superadmin(&args),
        "owner" => return parse_owner(&args),
        "access" => return parse_access(&args),
        "pair" | "devices" => return parse_management(&args),
        "recovery-export" | "recovery-import" | "backup-resolve" => return parse_recovery(&args),
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
    let mut agent_startup = None;
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
            "--agent-startup" if command == "create" => {
                agent_startup =
                    Some(AgentStartupChoice::parse(&text(value(&args, &mut at)?)?).ok_or(())?)
            }
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
                agent_startup,
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
        assert!(p(&["serve", "x", "--allow-local"]).is_ok());
        assert!(p(&["serve", "x", "--local-only", "--allow-local"]).is_err());
        for command in ["version", "update", "--help"] {
            assert!(p(&[command]).is_ok());
        }
        for action in ["init","status","check","stage","verify","configure","--help"] {
            assert!(matches!(p(&["update",action]),Ok(Command::Update(args)) if args==[action]));
            assert!(matches!(p(&["agent","update",action]),Ok(Command::Update(args)) if args==[action]));
        }
        assert!(p(&["update","apply"]).is_err());
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
        for mode in ["manual", "boot"] {
            let mut args = base.to_vec();
            args.extend(["--agent-startup", mode]);
            assert!(p(&args).is_ok());
        }
        let mut args = base.to_vec();
        args.extend(["--agent-startup", "login"]);
        assert!(p(&args).is_err());
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
    let import = args[0] == "recovery-import";
    let name = args.get(1).and_then(|v| v.to_str()).ok_or(())?.to_owned();
    if !valid_name(&name) {
        return Err(());
    }
    let (mut root, mut output, mut input, mut passphrase_file, mut confirm, mut job) =
        (None, None, None, None, None, None);
    let mut seen = std::collections::HashSet::new();
    let mut at = 2;
    if !export
        && !import
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
            "--input" if import => input = Some(PathBuf::from(value(args, &mut at)?)),
            "--passphrase-file" if export || import => {
                passphrase_file = Some(PathBuf::from(value(args, &mut at)?))
            }
            "--confirm" if !export => confirm = Some(text(value(args, &mut at)?)?),
            _ => return Err(()),
        }
        at += 1;
    }
    if export {
        Ok(Command::RecoveryExport {
            name,
            output: output.ok_or(())?,
            passphrase_file,
            root,
        })
    } else if import {
        Ok(Command::RecoveryImport {
            name,
            input: input.ok_or(())?,
            passphrase_file,
            confirm: confirm.ok_or(())?,
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
            Ok(Command::RecoveryExport {
                passphrase_file: None,
                ..
            })
        ));
        assert!(matches!(
            p(&[
                "recovery-export",
                "owned",
                "--output",
                "recovery.bin",
                "--passphrase-file",
                "secret.txt"
            ]),
            Ok(Command::RecoveryExport {
                passphrase_file: Some(_),
                ..
            })
        ));
        assert!(matches!(
            p(&[
                "recovery-import",
                "owned",
                "--input",
                "recovery.bin",
                "--confirm",
                "server-uuid"
            ]),
            Ok(Command::RecoveryImport {
                passphrase_file: None,
                ..
            })
        ));
        assert!(matches!(
            p(&[
                "recovery-import",
                "owned",
                "--input",
                "recovery.bin",
                "--passphrase-file",
                "secret.txt",
                "--confirm",
                "server-uuid",
                "--install-root",
                "root"
            ]),
            Ok(Command::RecoveryImport {
                passphrase_file: Some(_),
                ..
            })
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
            vec![
                "recovery-export",
                "owned",
                "--output",
                "out",
                "--passphrase-file",
            ],
            vec![
                "recovery-export",
                "owned",
                "--output",
                "out",
                "--passphrase-file",
                "pw",
                "--passphrase-file",
                "pw2",
            ],
            vec![
                "recovery-export",
                "owned",
                "--output",
                "out",
                "--passphrase",
                "secret",
            ],
            vec![
                "recovery-export",
                "owned",
                "--output",
                "out",
                "--passphrase-env",
                "SECRET",
            ],
            vec!["recovery-import"],
            vec![
                "recovery-import",
                "../owned",
                "--input",
                "in",
                "--confirm",
                "id",
            ],
            vec!["recovery-import", "owned", "--confirm", "id"],
            vec!["recovery-import", "owned", "--input", "in"],
            vec![
                "recovery-import",
                "owned",
                "--input",
                "in",
                "--confirm",
                "id",
                "--confirm",
                "id",
            ],
            vec![
                "recovery-import",
                "owned",
                "--input",
                "in",
                "--confirm",
                "id",
                "--passphrase",
                "secret",
            ],
            vec![
                "recovery-import",
                "owned",
                "--input",
                "in",
                "--confirm",
                "id",
                "--passphrase-env",
                "SECRET",
            ],
            vec![
                "recovery-import",
                "owned",
                "--input",
                "in",
                "--confirm",
                "id",
                "--input",
                "other",
            ],
            vec![
                "recovery-import",
                "owned",
                "--input",
                "in",
                "--confirm",
                "id",
                "--passphrase-file",
                "",
            ],
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
