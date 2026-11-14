//! Durable host policy projection. Worker databases never fall back to legacy
//! per-server identity grants, even before the first synchronization.
use super::*;
use crate::worker::{ServerRole, ServerSubject, WorkerPolicy};
use management::ManagementError;
use rusqlite::{Connection, OptionalExtension, params};
use vesta_state::StateDatabase;

pub(crate) fn initialize(
    db: &StateDatabase,
    host: uuid::Uuid,
    server: uuid::Uuid,
) -> std::result::Result<(), ManagementError> {
    db.write(|c| {
        c.execute_batch("CREATE TABLE IF NOT EXISTS worker_policy(singleton INTEGER PRIMARY KEY CHECK(singleton=1),host TEXT NOT NULL,server TEXT NOT NULL,generation INTEGER NOT NULL,document TEXT NOT NULL)")?;
        c.execute("INSERT OR IGNORE INTO worker_policy VALUES(1,?,?,0,'')", params![host.to_string(),server.to_string()])?;
        let binding:(String,String)=c.query_row("SELECT host,server FROM worker_policy WHERE singleton=1",[],|r|Ok((r.get(0)?,r.get(1)?)))?;
        if binding != (host.to_string(),server.to_string()) { return Err(vesta_state::StateError::new("worker_binding_mismatch")); }
        Ok(())
    }).map_err(|_|ManagementError::Unavailable)
}
pub(crate) fn sync(
    db: &StateDatabase,
    policy: &WorkerPolicy,
) -> std::result::Result<(), ManagementError> {
    if policy.generation == 0 || policy.generation > i64::MAX as u64 || policy.grants.len() > 4096 {
        return Err(ManagementError::InvalidRequest);
    }
    let mut devices = std::collections::HashSet::new();
    for g in &policy.grants {
        if g.account_id.is_nil()
            || g.device_id.is_nil()
            || !devices.insert(g.device_id)
            || g.capabilities.iter().any(|s| {
                !Capability::ALL
                    .iter()
                    .any(|c| c.id() == s && !c.local_only())
            })
        {
            return Err(ManagementError::InvalidRequest);
        }
    }
    let document = serde_json::to_string(policy).map_err(|_| ManagementError::InvalidRequest)?;
    db.write_with(|tx| -> std::result::Result<(), ManagementError> {
        let c = tx.connection();
        let (host, server, generation, previous): (String, String, i64, String) = c.query_row(
            "SELECT host,server,generation,document FROM worker_policy WHERE singleton=1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )?;
        if host != policy.host_id.to_string()
            || server != policy.server_id.to_string()
            || (policy.generation as i64) < generation
            || policy.generation as i64 == generation && previous != document
        {
            return Err(ManagementError::Forbidden);
        }
        c.execute(
            "UPDATE worker_policy SET generation=?,document=? WHERE singleton=1",
            params![policy.generation as i64, document],
        )?;
        Ok(())
    })
    .map_err(|_| ManagementError::Forbidden)
}
pub(crate) fn authenticate(
    db: &StateDatabase,
    s: &ServerSubject,
    subject: &Subject,
) -> std::result::Result<(), ManagementError> {
    db.read_with(|tx| -> std::result::Result<(), ManagementError> {
        let c = tx.connection();
        let generation: i64 = c.query_row(
            "SELECT generation FROM worker_policy WHERE singleton=1",
            [],
            |r| r.get(0),
        )?;
        if generation <= 0 || generation as u64 != s.generation {
            return Err(ManagementError::SessionInvalid);
        }
        if !resolve(c, db.server_id(), Some(subject))?.is_some_and(|p| p.has_current_identity()) {
            return Err(ManagementError::Forbidden);
        }
        Ok(())
    })
    .map_err(|_| ManagementError::SessionInvalid)
}
pub(crate) fn resolve(
    c: &Connection,
    _server: &str,
    subject: Option<&Subject>,
) -> crate::Result<Option<PolicySnapshot>> {
    let exists: bool = c.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='worker_policy')",
        [],
        |r| r.get(0),
    )?;
    if !exists {
        return Ok(None);
    }
    let document: Option<String> = c
        .query_row(
            "SELECT document FROM worker_policy WHERE singleton=1",
            [],
            |r| r.get(0),
        )
        .optional()?;
    let Some(subject) = subject else {
        return Ok(Some(PolicySnapshot::none()));
    };
    let Some(policy) = document.and_then(|s| serde_json::from_str::<WorkerPolicy>(&s).ok()) else {
        return Ok(Some(PolicySnapshot::none()));
    };
    let grant = policy.grants.iter().find(|g| {
        g.account_id.to_string() == subject.principal.as_str()
            && g.device_id.to_string() == subject.device.as_str()
    });
    let Some(grant) = grant else {
        return Ok(Some(PolicySnapshot::none()));
    };
    let builtin = match grant.role {
        ServerRole::Owner => BuiltinRole::Owner,
        ServerRole::Admin => BuiltinRole::Administrator,
        ServerRole::Moderator => BuiltinRole::Moderator,
        ServerRole::Player => BuiltinRole::PackUser,
    };
    let allows = Capability::ALL
        .iter()
        .filter(|c| {
            grant.capabilities.iter().any(|s| s == c.id()) && role_permits(&grant.role, **c)
        })
        .map(|c| RoleGrant::new(*c, ResourceScope::ALL))
        .collect::<crate::Result<Vec<_>>>()?;
    Ok(Some(PolicySnapshot::from_resolved(
        Some(subject.principal.clone()),
        Some(subject.device.clone()),
        builtin.rank(),
        vec![RoleId::parse(builtin.id())?],
        std::collections::HashSet::from([builtin]),
        allows,
        vec![],
    )))
}

/// Server roles never inherit host identity/security authority. These are upper
/// bounds; every action must also appear in the host's effective grant snapshot.
fn role_permits(role: &ServerRole, capability: Capability) -> bool {
    match role {
        ServerRole::Owner | ServerRole::Admin => {
            let builtin = if matches!(role, ServerRole::Owner) {
                BuiltinRole::Owner
            } else {
                BuiltinRole::Administrator
            };
            builtin.capabilities().contains(&capability)
                && !matches!(
                    capability,
                    Capability::DevicesRevoke
                        | Capability::RolesCreate
                        | Capability::CredentialsManage
                        | Capability::SecurityManage
                        | Capability::RolesManage
                        | Capability::AccessSourcesManage
                )
        }
        ServerRole::Moderator => matches!(
            capability,
            Capability::StatusView
                | Capability::PlayersView
                | Capability::PlayersModerate
                | Capability::ConsoleRead
                | Capability::LifecycleRestart
                | Capability::NotificationsView
                | Capability::TasksView
        ),
        ServerRole::Player => BuiltinRole::PackUser.capabilities().contains(&capability),
    }
}
