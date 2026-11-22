//! Host presets are copies. Applying a preset updates the same role ID on
//! the destination server and never modifies the source.
use super::{
    HostState,
    owners::Actor,
    registry::require_server,
    roles::{
        CustomRole, RoleFields, check_role_edit, decode, encode, prepare_fields, prepare_role,
        role_definition, role_summary, save_role,
    },
    state::{changed, expect_revision, reject, snapshot_in},
};
use crate::{IdentityErrorCode as Code, Result, identity::Capability};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

#[derive(Serialize, Deserialize)]
struct Preset {
    role: CustomRole,
    default_for_new_servers: bool,
}

impl Preset {
    fn applicable(&self) -> bool {
        prepare_role(RoleFields::default(), Some(self.role.clone())).is_ok()
    }

    fn summary(&self, id: &str) -> Value {
        json!({"id":id,"name":self.role.name,"rank":self.role.rank,
            "capabilities":self.role.capabilities,"default_for_new_servers":self.default_for_new_servers,
            "applicable":self.applicable()})
    }
}

fn load(c: &Connection, id: Uuid) -> Result<Preset> {
    let value: Option<String> = c
        .query_row(
            "SELECT document FROM machine_role_presets WHERE id=?",
            [id.to_string()],
            |row| row.get(0),
        )
        .optional()?;
    decode(&value.ok_or_else(|| reject(Code::NotFound))?)
}

fn save(c: &Connection, id: Uuid, preset: &Preset) -> Result<()> {
    if preset.default_for_new_servers && !preset.applicable() {
        return Err(reject(Code::Forbidden));
    }
    c.execute("INSERT INTO machine_role_presets VALUES(?,?) ON CONFLICT(id) DO UPDATE SET document=excluded.document",
        params![id.to_string(), encode(preset)?])?;
    Ok(())
}

fn list(c: &Connection) -> Result<Vec<(String, Preset)>> {
    let mut q = c.prepare("SELECT id,document FROM machine_role_presets ORDER BY id")?;
    let mut rows = q.query([])?;
    let mut presets = Vec::new();
    while let Some(row) = rows.next()? {
        presets.push((row.get(0)?, decode(&row.get::<_, String>(1)?)?));
    }
    Ok(presets)
}

fn authorize(c: &Connection, actor: Actor) -> Result<()> {
    super::access::require(c, actor, Capability::HostPresetsManage)
}

fn authorize_default(c: &Connection, actor: Actor, current: bool, proposed: bool) -> Result<()> {
    if (current || proposed) && !super::access::is_owner(c, actor)? {
        return Err(reject(Code::Forbidden));
    }
    Ok(())
}

pub(super) fn register_in(c: &Connection, server: Uuid) -> Result<()> {
    for (id, preset) in list(c)? {
        if preset.default_for_new_servers {
            save_role(c, server, &id, &preset.role)?;
        }
    }
    Ok(())
}

impl HostState {
    pub fn role_presets(&self, actor: Actor) -> Result<Value> {
        self.read(|c| {
            authorize(c, actor)?;
            Ok(json!({"version":snapshot_in(c)?.revision.to_string(),
                "presets":list(c)?.iter().map(|(id,preset)| preset.summary(id)).collect::<Vec<_>>() }))
        })
    }

    pub fn save_role_preset(
        &self,
        actor: Actor,
        server: Uuid,
        role_id: &str,
        revision: u64,
        default_for_new_servers: bool,
    ) -> Result<Value> {
        self.write(|c| {
            authorize(c, actor)?;
            authorize_default(c, actor, false, default_for_new_servers)?;
            expect_revision(c, revision)?;
            if !super::access::is_owner(c, actor)? {
                let Actor::Account(account) = actor else {
                    unreachable!()
                };
                if !super::roles::can_view(c, account, server)? {
                    return Err(reject(Code::Forbidden));
                }
            }
            let id = Uuid::new_v4();
            let preset = Preset {
                role: role_definition(c, server, role_id)?,
                default_for_new_servers,
            };
            save(c, id, &preset)?;
            let revision = changed(
                c,
                false,
                "role_preset_create",
                &actor.audit(),
                &id.to_string(),
                self.now_ms(),
            )?
            .revision;
            Ok(json!({"version":revision.to_string(),"preset":preset.summary(&id.to_string())}))
        })
    }

    pub fn patch_role_preset(
        &self,
        actor: Actor,
        id: Uuid,
        revision: u64,
        fields: RoleFields,
        default: Option<bool>,
    ) -> Result<Value> {
        self.write(|c| {
            authorize(c, actor)?;
            expect_revision(c, revision)?;
            let mut preset = load(c, id)?;
            let proposed = default.unwrap_or(preset.default_for_new_servers);
            authorize_default(c, actor, preset.default_for_new_servers, proposed)?;
            preset.role = prepare_fields(fields, Some(preset.role), true)?;
            preset.default_for_new_servers = proposed;
            save(c, id, &preset)?;
            let revision = changed(
                c,
                false,
                "role_preset_write",
                &actor.audit(),
                &id.to_string(),
                self.now_ms(),
            )?
            .revision;
            Ok(json!({"version":revision.to_string(),"preset":preset.summary(&id.to_string())}))
        })
    }

    pub fn delete_role_preset(&self, actor: Actor, id: Uuid, revision: u64) -> Result<Value> {
        self.write(|c| {
            authorize(c, actor)?;
            expect_revision(c, revision)?;
            authorize_default(c, actor, load(c, id)?.default_for_new_servers, false)?;
            c.execute(
                "DELETE FROM machine_role_presets WHERE id=?",
                [id.to_string()],
            )?;
            let revision = changed(
                c,
                false,
                "role_preset_delete",
                &actor.audit(),
                &id.to_string(),
                self.now_ms(),
            )?
            .revision;
            Ok(json!({"version":revision.to_string(),"id":id}))
        })
    }

    pub fn apply_role_preset(
        &self,
        actor: Actor,
        id: Uuid,
        server: Uuid,
        revision: u64,
    ) -> Result<Value> {
        self.write(|c| {
            authorize(c, actor)?;
            expect_revision(c, revision)?;
            require_server(c, server)?;
            let preset = load(c, id)?;
            if !preset.applicable() {
                return Err(reject(Code::Forbidden));
            }
            let id = id.to_string();
            let prior = super::roles::role_record(c, server, &id)?;
            check_role_edit(c, actor, server, &id, prior.as_ref(), &preset.role)?;
            save_role(c, server, &id, &preset.role)?;
            let revision = changed(
                c,
                true,
                "role_preset_apply",
                &actor.audit(),
                &format!("{server}/{id}"),
                self.now_ms(),
            )?
            .revision;
            Ok(json!({"version":revision.to_string(),"role":role_summary(c, server, &id)?}))
        })
    }
}
