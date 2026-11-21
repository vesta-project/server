//! Host invites grant authority on exactly one registered server.
use super::{
    HostState,
    accounts::*,
    owners::*,
    sessions::*,
    state::{changed, reject},
};
use crate::IdentityErrorCode as Code;
use base64::Engine;
use rusqlite::{OptionalExtension, params};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use uuid::Uuid;
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateInvite {
    pub role: String,
    pub expires_minutes: u16,
    #[serde(default)]
    pub multi_use: bool,
    pub max_uses: Option<u32>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RedeemInvite {
    pub invite: String,
    pub account_proof_id: String,
    pub device_key: String,
    pub signature: String,
    #[serde(default = "default_name")]
    pub device_name: String,
}
fn default_name() -> String {
    "Invited device".into()
}
fn ensure(c: &rusqlite::Connection) -> crate::Result<()> {
    c.execute_batch("CREATE TABLE IF NOT EXISTS machine_invites(id TEXT PRIMARY KEY,server TEXT NOT NULL,role TEXT NOT NULL,payload_hash TEXT NOT NULL UNIQUE,created INTEGER NOT NULL,expires INTEGER NOT NULL,creator TEXT NOT NULL,max_uses INTEGER NOT NULL,uses INTEGER NOT NULL DEFAULT 0,revoked INTEGER NOT NULL DEFAULT 0); CREATE TABLE IF NOT EXISTS machine_invite_receipts(invite TEXT NOT NULL,account TEXT NOT NULL,device TEXT NOT NULL,PRIMARY KEY(invite,device));")?;
    Ok(())
}
fn authority(
    c: &rusqlite::Connection,
    actor: Actor,
    server: Uuid,
    requested: Option<ServerRole>,
) -> crate::Result<()> {
    if !super::roles::can_invite_builtin(c, actor, server, requested)? {
        return Err(reject(Code::Forbidden));
    }
    Ok(())
}
pub fn issue(
    state: &HostState,
    actor: Actor,
    server: Uuid,
    fingerprint: &str,
    input: CreateInvite,
) -> crate::Result<Value> {
    let grant = ServerRole::parse(&input.role)?;
    if input.expires_minutes == 0 || input.expires_minutes > 10080 {
        return Err(reject(Code::InvalidRequest));
    }
    let max = if input.multi_use {
        input.max_uses.unwrap_or(100)
    } else {
        1
    };
    if max == 0 || max > 1000 {
        return Err(reject(Code::InvalidRequest));
    }
    let code = random().map_err(|_| reject(Code::InvalidRequest))?;
    let id = Uuid::new_v4();
    let now = state.now_ms();
    let expires = now + i64::from(input.expires_minutes) * 60000;
    let payload = json!({"v":1,"hostId":state.snapshot()?.host_id,"fingerprint":fingerprint,"serverId":server,"endpoints":[],"code":code,"expiresAt":chrono::DateTime::from_timestamp_millis(expires).unwrap().to_rfc3339_opts(chrono::SecondsFormat::Millis,true),"pack":null});
    let bytes = canonical(&payload);
    let hash = format!("{:x}", Sha256::digest(&bytes));
    let invite = format!(
        "vesta://invite/v1#{}",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&bytes)
    );
    state.write(|c| {authority(c,actor,server,Some(grant))?;ensure(c)?;c.execute("INSERT INTO machine_invites(id,server,role,payload_hash,created,expires,creator,max_uses) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",params![id.to_string(),server.to_string(),grant.as_str(),hash,now,expires,actor.audit(),max])?;changed(c,false,"invite_create",&actor.audit(),&id.to_string(),now)?;Ok(())})?;
    Ok(
        json!({"id":id,"inviteId":id,"code":invite,"invite":invite,"pairing_link":invite,"role":grant.id(),"expires_at_ms":expires}),
    )
}
pub fn list(state: &HostState, actor: Actor, server: Uuid) -> crate::Result<Value> {
    state.write(|c| {authority(c,actor,server,None)?;ensure(c)?;let mut q=c.prepare("SELECT id,role,created,expires,creator FROM machine_invites WHERE server=?1 AND revoked=0 AND expires>?2 AND uses<max_uses ORDER BY created,id LIMIT 100")?;
 let rows=q.query_map(params![server.to_string(),state.now_ms()],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,i64>(2)?,r.get::<_,i64>(3)?,r.get::<_,String>(4)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
 let invitations = rows.into_iter().map(|(id,role,created,expires,creator)| {
     Ok(json!({"id":id,"role":ServerRole::parse(&role)?.summary(),"created_at_ms":created,"expires_at_ms":expires,"created_by":creator}))
 }).collect::<crate::Result<Vec<_>>>()?;
 Ok(json!({"invitations":invitations,"next_offset":null}))})
}
pub fn revoke(state: &HostState, actor: Actor, server: Uuid, id: Uuid) -> crate::Result<()> {
    state.write(|c| {
        authority(c, actor, server, None)?;
        ensure(c)?;
        let grant: Option<String> = c
            .query_row(
                "SELECT role FROM machine_invites WHERE id=?1 AND server=?2",
                params![id.to_string(), server.to_string()],
                |r| r.get(0),
            )
            .optional()?;
        let grant = grant.ok_or_else(|| reject(Code::NotFound))?;
        let grant = ServerRole::parse(&grant)?;
        // Existing administrator invitations can still be revoked by an owner.
        if grant == ServerRole::Admin {
            if let Actor::Account(id) = actor {
                if host_role_in(c, id)?.is_none() {
                    return Err(reject(Code::Forbidden));
                }
            }
        } else {
            authority(c, actor, server, Some(grant))?;
        }
        c.execute(
            "UPDATE machine_invites SET revoked=1 WHERE id=?1",
            [id.to_string()],
        )?;
        changed(
            c,
            false,
            "invite_revoke",
            &actor.audit(),
            &id.to_string(),
            state.now_ms(),
        )?;
        Ok(())
    })
}
pub fn binding(invite: &str) -> Result<(Value, String)> {
    if invite.len() > 8192 {
        return Err(PAIRING);
    }
    use base64::Engine;
    let encoded = invite.strip_prefix("vesta://invite/v1#").ok_or(PAIRING)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(encoded)
        .map_err(|_| PAIRING)?;
    let value: Value = serde_json::from_slice(&bytes).map_err(|_| PAIRING)?;
    if canonical(&value) != bytes {
        return Err(PAIRING);
    }
    Ok((value, format!("{:x}", Sha256::digest(bytes))))
}
pub fn redeem(
    state: &HostState,
    accounts: &mut HostAccounts,
    scope: &AccountScope,
    fingerprint: &str,
    input: &RedeemInvite,
) -> Result<(super::signin::SignedIn, Uuid, String)> {
    let (payload, hash) = binding(&input.invite)?;
    let host = state.snapshot().map_err(|_| UNAVAILABLE)?.host_id;
    if scope.purpose != AccountPurpose::Invite
        || scope.binding != hash
        || scope.device_key != input.device_key
        || payload["hostId"] != host.to_string()
        || payload["fingerprint"] != fingerprint
    {
        return Err(PAIRING);
    }
    let server = payload["serverId"]
        .as_str()
        .and_then(|s| Uuid::parse_str(s).ok())
        .ok_or(PAIRING)?;
    let key = decode::<32>(&input.device_key)?;
    let name = super::settings::device_name(&input.device_name).ok_or(PAIRING)?;
    verify(
        &input.device_key,
        &input.signature,
        "vesta.host.invite.v1",
        &json!({"hostId":host,"serverId":server,"inviteHash":hash,"accountProofId":input.account_proof_id,"deviceKey":input.device_key}),
    )?;
    accounts.consume_checked(&input.account_proof_id,scope,|account,verified_name,live|state.write(|c| {
  ensure(c)?;super::registry::require_server(c,server)?;
  if !live(){return Err(reject(Code::CodeInvalid))}
  let row:Option<(String,String,i64,u32,u32,bool)>=c.query_row("SELECT id,role,expires,max_uses,uses,revoked FROM machine_invites WHERE payload_hash=?1 AND server=?2",params![hash,server.to_string()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?))).optional()?;
  let (id,grant,expires,max,used,revoked)=row.ok_or_else(||reject(Code::CodeInvalid))?;
  if revoked || expires<=state.now_ms(){return Err(reject(Code::CodeInvalid))}
  let existing=device_by_key_in(c,&key)?;
  if existing.as_ref().is_some_and(|d|d.account_id!=account || d.revoked_at_ms.is_some()){return Err(reject(Code::Forbidden))}
  let replay=if let Some(d)=&existing { c.query_row("SELECT EXISTS(SELECT 1 FROM machine_invite_receipts WHERE invite=?1 AND account=?2 AND device=?3)",params![id,account.to_string(),d.device_id.to_string()],|r|r.get::<_,bool>(0))? } else {false};
  if !replay && used>=max {return Err(reject(Code::CodeInvalid))}
  let created=existing.is_none();let device=match existing {Some(d)=>d,None=>enroll_device_in(c,account,&name,key,state.now_ms())?};
  if !replay {
   let desired=ServerRole::parse(&grant)?;
   // Invite redemption never downgrades existing authority or changes owners.
   if host_role_in(c,account)?.is_none() {
    let current=super::registry::effective_role_in(c,account,server)?;
    let keeps_current = current.is_some_and(|role| role.rank() >= desired.rank());
    if !keeps_current {super::roles::clear_overrides(c,server,account)?;c.execute("INSERT INTO machine_server_grants(server_id,account_id,role) VALUES(?1,?2,?3) ON CONFLICT(server_id,account_id) DO UPDATE SET role=excluded.role",params![server.to_string(),account.to_string(),grant])?;}
   }
   c.execute("UPDATE machine_invites SET uses=uses+1 WHERE id=?1",[&id])?;
   c.execute("INSERT INTO machine_invite_receipts VALUES(?1,?2,?3)",params![id,account.to_string(),device.device_id.to_string()])?;
   changed(c,true,"invite_redeem",&account.to_string(),&server.to_string(),state.now_ms())?;
  }
  record_account_name_in(c,account,verified_name,state.now_ms())?;
  Ok((super::signin::SignedIn{device,created},server,grant))
 }).map_err(|_|PAIRING))
}
