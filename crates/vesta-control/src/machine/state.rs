use crate::{ControlError, IdentityErrorCode, Result};
use rusqlite::{Connection, OptionalExtension, params};
use std::{path::Path, sync::Arc};
use uuid::Uuid;
use vesta_state::{Clock, StateDatabase, StateDirectory, StateError};

pub const HOST_SCHEMA_VERSION: u32 = 5;

/// A separate protected state directory is required, outside every game tree.
/// The underlying vesta-state identity is storage infrastructure, never a host id
/// or a registry server id. Host schema revisions are independent of its schema.
#[derive(Clone, Debug)]
pub struct HostState {
    database: StateDatabase,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostSnapshot {
    pub host_id: Uuid,
    pub schema_version: u32,
    pub revision: u64,
    pub auth_generation: u64,
    pub primary_account_id: Option<Uuid>,
}

impl HostState {
    pub fn open(path: impl AsRef<Path>, clock: Arc<dyn Clock>) -> Result<Self> {
        Self::from_database(StateDatabase::open(
            StateDirectory::new(path, None)?,
            clock,
        )?)
    }

    /// Use only a dedicated host database, never an existing server database.
    pub fn from_database(database: StateDatabase) -> Result<Self> {
        let state = Self { database };
        state.write(|c| {
            c.execute_batch(SCHEMA)?;
            c.execute("INSERT OR IGNORE INTO machine_host(singleton,host_id,schema_version,revision,auth_generation) VALUES(1,?1,1,1,1)", [Uuid::new_v4().to_string()])?;
            if snapshot_in(c)?.schema_version == 1 {
                migrate_server_roles(c)?;
            }
            if snapshot_in(c)?.schema_version == 2 {
                migrate_custom_roles(c)?;
            }
            if snapshot_in(c)?.schema_version == 3 {
                c.execute_batch(
                    "ALTER TABLE machine_servers ADD COLUMN enabled INTEGER NOT NULL DEFAULT 1 CHECK(enabled IN (0,1));
                     CREATE TABLE machine_host_capabilities(
                         account_id TEXT PRIMARY KEY REFERENCES machine_accounts(account_id), document TEXT NOT NULL);
                     CREATE TABLE machine_role_presets(id TEXT PRIMARY KEY, document TEXT NOT NULL);
                     UPDATE machine_host SET schema_version=4, auth_generation=auth_generation+1 WHERE singleton=1;"
                )?;
            }
            if snapshot_in(c)?.schema_version == 4 {
                migrate_default_access(c)?;
            }
            if snapshot_in(c)?.schema_version != HOST_SCHEMA_VERSION {
                return Err(StateError::new("state_host_schema_unsupported").into());
            }
            Ok(())
        })?;
        Ok(state)
    }

    pub fn snapshot(&self) -> Result<HostSnapshot> {
        self.read(snapshot_in)
    }
    pub fn database(&self) -> &StateDatabase {
        &self.database
    }
    pub fn now_ms(&self) -> i64 {
        self.database.clock().millis()
    }

    /// Trusted repository seam for atomic proof/code consumption with enrollment.
    /// Callback must not end the transaction or execute caller-selected SQL.
    pub fn write<T>(&self, work: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        self.database
            .write_with(|tx| work(tx.connection()))
            .map_err(ControlError::from)
    }
    pub fn read<T>(&self, work: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        self.database
            .read_with(|tx| work(tx.connection()))
            .map_err(ControlError::from)
    }
}

fn migrate_default_access(c: &Connection) -> Result<()> {
    // Older role removals left an audit record and no grant. Carry those
    // exclusions forward before an owner can enable fallback access.
    c.execute_batch(
        "ALTER TABLE machine_servers ADD COLUMN default_role_id TEXT;
         CREATE TABLE machine_server_access_exclusions(
             server_id TEXT NOT NULL REFERENCES machine_servers(server_id),
             account_id TEXT NOT NULL REFERENCES machine_accounts(account_id),
             PRIMARY KEY(server_id,account_id));
         INSERT OR IGNORE INTO machine_server_access_exclusions(server_id,account_id)
             SELECT s.server_id,a.account_id FROM machine_audit log
             JOIN machine_servers s ON s.server_id=substr(log.target,1,36)
             JOIN machine_accounts a ON a.account_id=substr(log.target,38)
             WHERE log.action='server_role_change' AND substr(log.target,37,1)='/'
               AND s.state!='deleted' AND NOT EXISTS (
                   SELECT 1 FROM machine_server_grants g
                   WHERE g.server_id=s.server_id AND g.account_id=a.account_id);
         UPDATE machine_servers SET visibility='private' WHERE visibility='public';
         UPDATE machine_host SET schema_version=5, auth_generation=auth_generation+1 WHERE singleton=1;",
    )?;
    Ok(())
}

fn migrate_server_roles(c: &Connection) -> Result<()> {
    c.execute_batch(
        "CREATE TABLE machine_server_grants_v2(
            server_id TEXT NOT NULL REFERENCES machine_servers(server_id),
            account_id TEXT NOT NULL REFERENCES machine_accounts(account_id),
            role TEXT NOT NULL CHECK(role IN ('admin','maintainer','moderator','viewer','pack-user','player')),
            PRIMARY KEY(server_id,account_id));
         INSERT INTO machine_server_grants_v2
             SELECT server_id,account_id,CASE role WHEN 'player' THEN 'pack-user' ELSE role END
             FROM machine_server_grants;
         DROP TABLE machine_server_grants;
         ALTER TABLE machine_server_grants_v2 RENAME TO machine_server_grants;",
    )?;
    let invites_exist = c.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='machine_invites')",
        [],
        |row| row.get::<_, bool>(0),
    )?;
    if invites_exist {
        c.execute(
            "UPDATE machine_invites SET role='pack-user' WHERE role='player'",
            [],
        )?;
    }
    // Old Player granted Pack User capabilities. Preserve those grants and
    // fence worker projections that used the previous role IDs.
    c.execute("UPDATE machine_host SET schema_version=2, auth_generation=auth_generation+1 WHERE singleton=1 AND auth_generation<9223372036854775807", [])?;
    Ok(())
}

fn migrate_custom_roles(c: &Connection) -> Result<()> {
    c.execute_batch(
        "CREATE TABLE machine_roles(
             server_id TEXT NOT NULL REFERENCES machine_servers(server_id),
             id TEXT NOT NULL, document TEXT NOT NULL, PRIMARY KEY(server_id,id));
         CREATE TABLE machine_server_grants_v3(
             server_id TEXT NOT NULL REFERENCES machine_servers(server_id),
             account_id TEXT NOT NULL REFERENCES machine_accounts(account_id),
             role TEXT NOT NULL CHECK(role IN ('admin','maintainer','moderator','viewer','pack-user','player') OR length(role)=36),
             PRIMARY KEY(server_id,account_id));
         INSERT INTO machine_server_grants_v3 SELECT * FROM machine_server_grants;
         DROP TABLE machine_server_grants;
         ALTER TABLE machine_server_grants_v3 RENAME TO machine_server_grants;
         CREATE TABLE machine_capability_overrides(
             server_id TEXT NOT NULL, account_id TEXT NOT NULL, document TEXT NOT NULL,
             PRIMARY KEY(server_id,account_id),
             FOREIGN KEY(server_id,account_id) REFERENCES machine_server_grants(server_id,account_id) ON DELETE CASCADE);
         UPDATE machine_host SET schema_version=3, auth_generation=auth_generation+1 WHERE singleton=1;",
    )?;
    Ok(())
}

pub(crate) fn reject(code: IdentityErrorCode) -> ControlError {
    ControlError::rejected(code)
}
pub(crate) fn uuid(value: String) -> Result<Uuid> {
    Uuid::parse_str(&value).map_err(|_| StateError::new("state_host_record_invalid").into())
}
pub(crate) fn snapshot_in(c: &Connection) -> Result<HostSnapshot> {
    let (id, schema, revision, generation, primary): (String,u32,i64,i64,Option<String>) = c.query_row(
        "SELECT host_id,schema_version,revision,auth_generation,primary_account_id FROM machine_host WHERE singleton=1", [],
        |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?)))?;
    Ok(HostSnapshot {
        host_id: uuid(id)?,
        schema_version: schema,
        revision: revision
            .try_into()
            .map_err(|_| StateError::new("state_host_record_invalid"))?,
        auth_generation: generation
            .try_into()
            .map_err(|_| StateError::new("state_host_record_invalid"))?,
        primary_account_id: primary.map(uuid).transpose()?,
    })
}
pub(crate) fn expect_revision(c: &Connection, expected: u64) -> Result<()> {
    if snapshot_in(c)?.revision != expected {
        return Err(reject(IdentityErrorCode::Conflict));
    }
    Ok(())
}
pub(crate) fn changed(
    c: &Connection,
    authorization: bool,
    action: &str,
    actor: &str,
    target: &str,
    now: i64,
) -> Result<HostSnapshot> {
    let old = snapshot_in(c)?;
    if old.revision >= i64::MAX as u64 || (authorization && old.auth_generation >= i64::MAX as u64)
    {
        return Err(StateError::new("state_host_counter_exhausted").into());
    }
    c.execute("UPDATE machine_host SET revision=revision+1,auth_generation=auth_generation+?1 WHERE singleton=1", [i64::from(authorization)])?;
    c.execute("INSERT INTO machine_audit(action,actor,target,at_ms,revision,auth_generation) SELECT ?1,?2,?3,?4,revision,auth_generation FROM machine_host WHERE singleton=1", params![action,actor,target,now])?;
    snapshot_in(c)
}
pub(crate) fn account_exists(c: &Connection, account: Uuid) -> Result<bool> {
    Ok(c.query_row(
        "SELECT 1 FROM machine_accounts WHERE account_id=?1 AND disabled=0",
        [account.to_string()],
        |_| Ok(()),
    )
    .optional()?
    .is_some())
}

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS machine_accounts(account_id TEXT PRIMARY KEY, disabled INTEGER NOT NULL DEFAULT 0 CHECK(disabled IN (0,1)));
CREATE TABLE IF NOT EXISTS machine_host(
 singleton INTEGER PRIMARY KEY CHECK(singleton=1), host_id TEXT NOT NULL UNIQUE,
 schema_version INTEGER NOT NULL, revision INTEGER NOT NULL CHECK(revision>0),
 auth_generation INTEGER NOT NULL CHECK(auth_generation>0),
 primary_account_id TEXT REFERENCES machine_accounts(account_id),
 claimed INTEGER NOT NULL DEFAULT 0 CHECK(claimed IN (0,1)),
 CHECK((claimed=0 AND primary_account_id IS NULL) OR (claimed=1 AND primary_account_id IS NOT NULL)));
CREATE TRIGGER IF NOT EXISTS machine_no_unclaim BEFORE UPDATE OF claimed ON machine_host WHEN OLD.claimed=1 AND NEW.claimed=0 BEGIN SELECT RAISE(ABORT,'host_already_claimed'); END;
CREATE TRIGGER IF NOT EXISTS machine_no_host_delete BEFORE DELETE ON machine_host BEGIN SELECT RAISE(ABORT,'host_identity_immutable'); END;
CREATE TABLE IF NOT EXISTS machine_owners(account_id TEXT PRIMARY KEY REFERENCES machine_accounts(account_id));
CREATE TABLE IF NOT EXISTS machine_devices(device_id TEXT PRIMARY KEY, account_id TEXT NOT NULL REFERENCES machine_accounts(account_id), name TEXT NOT NULL,
 public_key BLOB NOT NULL UNIQUE CHECK(length(public_key)=32), created_at_ms INTEGER NOT NULL, revoked_at_ms INTEGER);
CREATE TRIGGER IF NOT EXISTS machine_device_binding BEFORE UPDATE OF account_id,public_key ON machine_devices BEGIN SELECT RAISE(ABORT,'device_binding_immutable'); END;
CREATE TABLE IF NOT EXISTS machine_servers(server_id TEXT PRIMARY KEY, name TEXT NOT NULL, game_port INTEGER NOT NULL CHECK(game_port BETWEEN 1 AND 65535),
 state TEXT NOT NULL CHECK(state IN ('registered','starting','online','stopped','sleeping','failed','deleted')),
 visibility TEXT NOT NULL CHECK(visibility IN ('private','public')));
CREATE UNIQUE INDEX IF NOT EXISTS machine_game_port ON machine_servers(game_port) WHERE state!='deleted';
CREATE TABLE IF NOT EXISTS machine_server_grants(server_id TEXT NOT NULL REFERENCES machine_servers(server_id), account_id TEXT NOT NULL REFERENCES machine_accounts(account_id),
 role TEXT NOT NULL CHECK(role IN ('admin','moderator','player')), PRIMARY KEY(server_id,account_id));
CREATE TABLE IF NOT EXISTS machine_audit(sequence INTEGER PRIMARY KEY,action TEXT NOT NULL,actor TEXT NOT NULL,target TEXT NOT NULL,at_ms INTEGER NOT NULL,revision INTEGER NOT NULL,auth_generation INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS machine_startup(server_id TEXT PRIMARY KEY REFERENCES machine_servers(server_id), document TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS machine_startup_failures(server_id TEXT PRIMARY KEY REFERENCES machine_servers(server_id), document TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS machine_settings(key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS machine_account_names(account_id TEXT PRIMARY KEY REFERENCES machine_accounts(account_id), name TEXT NOT NULL, verified_at_ms INTEGER NOT NULL);
";

#[cfg(test)]
mod role_migration_tests {
    use super::*;

    #[test]
    fn old_player_grants_and_invites_become_pack_user_and_fence_workers() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(SCHEMA).unwrap();
        c.execute_batch(
            "INSERT INTO machine_host(singleton,host_id,schema_version,revision,auth_generation) VALUES(1,'h',1,1,5);
             INSERT INTO machine_accounts(account_id) VALUES('a'),('b');
             INSERT INTO machine_servers VALUES('s','Survival',25565,'stopped','private');
             INSERT INTO machine_server_grants VALUES('s','a','player'),('s','b','admin');
             CREATE TABLE machine_invites(id TEXT PRIMARY KEY,server TEXT NOT NULL,role TEXT NOT NULL);
             INSERT INTO machine_invites VALUES('i','s','player');",
        )
        .unwrap();
        migrate_server_roles(&c).unwrap();
        let role = |account: &str| -> String {
            c.query_row(
                "SELECT role FROM machine_server_grants WHERE account_id=?1",
                [account],
                |r| r.get(0),
            )
            .unwrap()
        };
        assert_eq!(role("a"), "pack-user");
        assert_eq!(role("b"), "admin");
        let invite: String = c
            .query_row("SELECT role FROM machine_invites", [], |r| r.get(0))
            .unwrap();
        assert_eq!(invite, "pack-user");
        let (version, generation): (u32, i64) = c
            .query_row(
                "SELECT schema_version,auth_generation FROM machine_host",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!((version, generation), (2, 6));
        // The new roles are accepted by the migrated table.
        c.execute(
            "UPDATE machine_server_grants SET role='maintainer' WHERE account_id='a'",
            [],
        )
        .unwrap();
        assert!(
            c.execute(
                "UPDATE machine_server_grants SET role='owner' WHERE account_id='a'",
                []
            )
            .is_err()
        );
    }
}
