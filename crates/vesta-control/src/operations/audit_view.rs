//! Allowlisted remote audit projection: identity labels and typed targets only.
use super::activity::{ActivityActor, actor};
use super::{OperationError, Operations, OperationsResult, domain::invalid};
use crate::identity::{self, Capability, Subject};
use serde::Serialize;

#[derive(Serialize)]
pub struct AuditNotice {
    pub cursor: String,
    pub at_ms: i64,
    pub action: &'static str,
    pub outcome: &'static str,
    pub actor: ActivityActor,
    pub target: Option<String>,
}
impl Operations {
    pub fn audit_notices(
        &self,
        subject: &Subject,
        after: i64,
        limit: usize,
    ) -> OperationsResult<Vec<AuditNotice>> {
        if after < 0 || !(1..=200).contains(&limit) {
            return Err(invalid());
        }
        self.db.read_with(|snapshot| {
            let policy = identity::policy_in(snapshot.connection(), self.db.server_id(), self.db.clock().millis(), Some(subject))?;
            if let identity::Decision::Deny(reason) = policy.check(Capability::AuditView, None) { return Err(OperationError::forbidden(reason)); }
            let mut statement = snapshot.connection().prepare("SELECT seq, at, action, outcome, substr(principal_id,1,128), substr(device_id,1,128), substr(source,1,64), CASE WHEN action IN ('files.write','files.delete','files.restore','files.rename','files.mkdir') AND json_valid(metadata_json) THEN substr(json_extract(metadata_json,'$.path'),1,512) END, (SELECT substr(name,1,128) FROM roles WHERE a.action='management.permissions.mutate' AND a.target='roles/'||id), (SELECT player FROM moderation_actions WHERE server_id=a.server_id AND operation_id=a.operation_id LIMIT 1) FROM audit_events a WHERE server_id = ? AND seq > ? ORDER BY seq LIMIT ?")?;
            let after = if after == 0 {
                snapshot.connection().query_row("SELECT COALESCE((SELECT seq FROM audit_events WHERE server_id = ? ORDER BY seq DESC LIMIT 1 OFFSET ?), 0)", rusqlite::params![self.db.server_id(), limit as i64], |row| row.get::<_, i64>(0))?
            } else { after };
            let rows = statement.query_map(rusqlite::params![self.db.server_id(), after, limit as i64], |row| {
                let action: String = row.get(2)?;
                let outcome: String = row.get(3)?;
                let target: Option<String> = row.get(7)?;
                let role: Option<String> = row.get(8)?;
                let player: Option<String> = row.get(9)?;
                let target = if let Some(role) = role.filter(|v| crate::identity::RoleName::parse(v).is_ok()) { Some(role) } else if let Some(player) = player.filter(|v| policy.check(Capability::PlayersView,None).allowed() && crate::moderation::valid_player(v)) { Some(player) } else if action == "config.server_properties" { Some("server.properties".into()) } else { target.filter(|path| !path.contains("[abbrev]") && crate::files::RelativePath::parse(path).is_ok()) };
                Ok((row.get::<_,String>(4)?,row.get::<_,String>(5)?,row.get::<_,String>(6)?,AuditNotice { cursor: row.get::<_, i64>(0)?.to_string(), at_ms: row.get(1)?, actor: ActivityActor::Automation { kind:"system", name:"Vesta agent" }, target, action: match action.as_str() {
                    "operation.submit" => "operation.submit",
                    "operation.transition" => "operation.transition",
                    "confirmation.issue" => "confirmation.issue",
                    "confirmation.consume" => "confirmation.consume",
                    "pack_release.publish" => "pack_release.publish",
                    "pack_release.minimum" => "pack_release.minimum",
                    "console.command" => "console.command",
                    "operation.finish" => "operation.finish",
                    "content.install" => "content.install",
                    "files.write" => "files.write",
                    "files.delete" => "files.delete",
                    "files.restore" => "files.restore",
                    "files.rename" => "files.rename",
                    "files.mkdir" => "files.mkdir",
                    "files.upload.begin" => "files.upload.begin",
                    "files.upload.append" => "files.upload.append",
                    "files.upload.abort" => "files.upload.abort",
                    "config.server_properties" => "config.server_properties",
                    "management.permissions.mutate" => "management.permissions.mutate",
                    "management.superadmin.grant" => "management.superadmin.grant",
                    "management.superadmin.revoke" => "management.superadmin.revoke",
                    "confirmation.accept" => "confirmation.accept",
                    _ => "other",
                }, outcome: match outcome.as_str() {
                    "succeeded" => "succeeded", "recovery_required" => "recovery_required", "cancelled" => "cancelled", "ok" => "ok", "accepted" => "accepted", "success" => "success", "denied" => "denied", "failed" => "failed", _ => "other",
                } }))
            })?;
            let mut notices = Vec::new();
            for row in rows {
                let (principal,device,source,mut notice) = row?;
                notice.actor = actor(snapshot.connection(),self.db.server_id(),&principal,&device,&source)?;
                notices.push(notice);
            }
            Ok(notices)
        }).map_err(OperationError::from)
    }
}
