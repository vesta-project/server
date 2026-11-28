//! Durable host policy projection. Worker databases never fall back to legacy
//! per-server identity grants, even before the first synchronization.
use super::*;
use crate::worker::{ServerSubject, WorkerPolicy};
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
    if policy.access_sources_accounts.len() > 4096
        || policy.access_sources_accounts.iter().any(|id| id.is_nil())
    {
        return Err(ManagementError::InvalidRequest);
    }
    let mut devices = std::collections::HashSet::new();
    if policy.whitelist_owners.len() > 4096
        || policy.whitelist_owners.iter().any(|(id, name)| id.is_nil() || !crate::moderation::valid_player(name))
    {
        return Err(ManagementError::InvalidRequest);
    }
    if policy.whitelist_roles.len() > 128
        || policy.whitelist_roles.keys().any(|id| RoleId::parse(id).is_err())
        || policy.whitelist_roles.values().map(|members| members.len()).sum::<usize>() > 4096
        || policy.whitelist_roles.values().flat_map(|members| members.iter()).any(|(id, name)| id.is_nil() || !crate::moderation::valid_player(name))
        || policy.activity_identities.len() > policy.grants.len()
        || policy.activity_identities.values().any(|labels| {
            DisplayName::parse(&labels.display_name).is_err()
                || DeviceLabel::parse(&labels.device_name).is_err()
        })
    {
        return Err(ManagementError::InvalidRequest);
    }
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
                && !serde_json::from_str::<WorkerPolicy>(&previous)
                    .is_ok_and(|prior| prior.grants == policy.grants
                        && prior.access_sources_accounts == policy.access_sources_accounts)
        {
            return Err(ManagementError::Forbidden);
        }
        if previous == document {
            return Ok(());
        }
        for grant in &policy.grants {
            if let Some(labels) = policy.activity_identities.get(&grant.device_id) {
                c.execute(
                    "INSERT INTO worker_activity_identities VALUES(?,?,?,?)
                     ON CONFLICT(account_id,device_id) DO UPDATE SET
                        display_name=excluded.display_name,device_name=excluded.device_name",
                    params![grant.account_id.to_string(), grant.device_id.to_string(),
                        labels.display_name, labels.device_name],
                )?;
            }
        }
        c.execute(
            "UPDATE worker_policy SET generation=?,document=? WHERE singleton=1",
            params![policy.generation as i64, document],
        )?;
        Ok(())
    })
    .map_err(|_| ManagementError::Forbidden)
}

fn policy_document(c: &Connection) -> vesta_state::Result<Option<String>> {
    let exists: bool = c.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name='worker_policy')",
        [],
        |row| row.get(0),
    )?;
    if !exists {
        return Ok(None);
    }
    Ok(Some(c.query_row(
        "SELECT document FROM worker_policy WHERE singleton=1",
        [],
        |row| row.get(0),
    )?))
}

pub(crate) fn whitelist_sync_owner(c: &Connection) -> vesta_state::Result<Option<Subject>> {
    let Some(document) = policy_document(c)? else {
        return Ok(None);
    };
    Ok(serde_json::from_str::<WorkerPolicy>(&document).ok().and_then(|policy| {
        let grant = policy.grants.iter().find(|grant| {
            grant.role == crate::worker::ServerRole::Owner
                && grant.capabilities.iter().any(|cap| cap == Capability::PlayersModerate.id())
        })?;
        Some(Subject {
            principal: PrincipalId::parse(&grant.account_id.to_string()).ok()?,
            device: DeviceId::parse(&grant.device_id.to_string()).ok()?,
        })
    }))
}

pub(crate) fn whitelist_roles(
    c: &Connection,
) -> vesta_state::Result<Option<crate::worker::WhitelistRoles>> {
    let Some(document) = policy_document(c)? else {
        return Ok(None);
    };
    let policy: WorkerPolicy = serde_json::from_str(&document)
        .map_err(|_| vesta_state::StateError::new("worker_policy_unavailable"))?;
    let mut roles = policy.whitelist_roles;
    roles.entry("owner".into()).or_insert(policy.whitelist_owners);
    // Older snapshots did not enumerate valid role IDs.
    for role in crate::machine::owners::ServerRole::ALL {
        roles.entry(role.id().into()).or_default();
    }
    Ok(Some(roles))
}
/// Old removals could persist an empty policy one generation ahead of the
/// Host before committing the deletion. A new worker for a registered server
/// with a pending receipt waits for the current Host policy after this reset.
pub(crate) fn recover_removal_fence(db: &StateDatabase, generation: u64) -> vesta_state::Result<()> {
    let Ok(fence) = i64::try_from(generation.saturating_add(1)) else {
        return Ok(());
    };
    db.write(|c| {
        let document: Option<String> = c.query_row(
            "SELECT document FROM worker_policy WHERE singleton=1 AND generation=?",
            [fence],
            |row| row.get(0),
        ).optional()?;
        if document.and_then(|value| serde_json::from_str::<WorkerPolicy>(&value).ok())
            .is_some_and(|policy| policy.grants.is_empty() && policy.whitelist_owners.is_empty())
        {
            c.execute("UPDATE worker_policy SET generation=0,document='' WHERE singleton=1", [])?;
        }
        Ok(())
    })
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
    let server_role = grant.role.server_role();
    let (builtin, rank, id) = match grant.role {
        crate::worker::ServerRole::Custom { id, rank } => (None, rank, id.to_string()),
        crate::worker::ServerRole::EditedBuiltin { ref role, rank } => (None, rank, role.clone()),
        _ => {
            let builtin = server_role.map_or(BuiltinRole::Owner, |role| role.builtin());
            (Some(builtin), builtin.rank(), builtin.id().into())
        }
    };
    let access_sources = matches!(grant.role, crate::worker::ServerRole::Owner)
        || policy.access_sources_accounts.contains(&grant.account_id);
    let allows = grant.capabilities.iter()
        .filter_map(|id| Capability::from_id(id))
        .filter(|&cap| {
            if cap == Capability::AccessSourcesManage {
                return access_sources;
            }
            grant.role.permits_projection(cap)
                && (builtin.is_some_and(|role| role.capabilities().contains(&cap))
                    || crate::machine::roles::host_delegable(cap))
        })
        .map(|c| RoleGrant::new(c, ResourceScope::ALL))
        .collect::<crate::Result<Vec<_>>>()?;
    Ok(Some(PolicySnapshot::from_resolved(
        Some(subject.principal.clone()),
        Some(subject.device.clone()),
        rank,
        vec![RoleId::parse(&id)?],
        builtin.into_iter().collect(),
        allows,
        vec![],
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::worker::ServerRole;
    use std::sync::Arc;
    use uuid::Uuid;

    #[test]
    fn recovers_only_the_empty_legacy_removal_fence() {
        let tmp = tempfile::tempdir().unwrap();
        let db = StateDatabase::open(
            vesta_state::StateDirectory::new(tmp.path().join("worker"), None).unwrap(),
            Arc::new(vesta_state::SystemClock),
        ).unwrap();
        let mut policy = WorkerPolicy {
            host_id: Uuid::new_v4(),
            server_id: Uuid::new_v4(),
            generation: 4,
            grants: vec![],
            whitelist_owners: Default::default(),
            whitelist_roles: Default::default(),
            activity_identities: Default::default(),
            access_sources_accounts: Default::default(),
        };
        initialize(&db, policy.host_id, policy.server_id).unwrap();
        sync(&db, &policy).unwrap();
        let generation = || db.read(|c| Ok(c.query_row("SELECT generation FROM worker_policy", [], |row| row.get::<_, i64>(0))?)).unwrap();
        recover_removal_fence(&db, 2).unwrap();
        assert_eq!(generation(), 4);
        recover_removal_fence(&db, 3).unwrap();
        assert_eq!(generation(), 0);
        policy.generation = 3;
        policy.grants.push(crate::worker::WorkerGrant {
            account_id: Uuid::new_v4(),
            device_id: Uuid::new_v4(),
            role: ServerRole::Owner,
            capabilities: vec![Capability::StatusView.id().into()],
        });
        sync(&db, &policy).unwrap();
        recover_removal_fence(&db, 2).unwrap();
        assert_eq!(generation(), 3);
    }
}
