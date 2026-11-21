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
    let mut devices = std::collections::HashSet::new();
    if policy.whitelist_owners.len() > 4096
        || policy.whitelist_owners.iter().any(|(id, name)| id.is_nil() || !crate::moderation::valid_player(name))
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
                    .is_ok_and(|prior| prior.grants == policy.grants)
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

pub(crate) fn whitelist_owners(
    c: &Connection,
) -> vesta_state::Result<Option<std::collections::BTreeMap<uuid::Uuid, String>>> {
    let exists: bool = c.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='worker_policy')",
        [],
        |row| row.get(0),
    )?;
    if !exists {
        return Ok(None);
    }
    let document: String = c.query_row("SELECT document FROM worker_policy WHERE singleton=1", [], |row| row.get(0))?;
    let owners = serde_json::from_str::<WorkerPolicy>(&document).map(|policy| policy.whitelist_owners).unwrap_or_default();
    Ok(Some(owners))
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
    let builtin = Some(server_role.map_or(BuiltinRole::Owner, |role| role.builtin()));
    let allows = grant.role.capabilities()
        .filter(|c| grant.capabilities.iter().any(|s| s == c.id()))
        .map(|c| RoleGrant::new(c, ResourceScope::ALL))
        .collect::<crate::Result<Vec<_>>>()?;
    Ok(Some(PolicySnapshot::from_resolved(
        Some(subject.principal.clone()),
        Some(subject.device.clone()),
        server_role.map_or(BuiltinRole::Owner.rank(), |role| role.rank()),
        vec![RoleId::parse(server_role.map_or(BuiltinRole::Owner.id(), |role| role.id()))?],
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
