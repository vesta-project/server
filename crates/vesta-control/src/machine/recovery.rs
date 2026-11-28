//! Privileged local recovery. This module has no HTTP or bearer entry point.
use super::{
    HostState,
    claim::{receipt, save_receipt, schema},
    owners::Device,
    sessions::*,
};
use rusqlite::params;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;
/// Trusted OS adapter capability, never a wire field. H11 replaces the first-pass
/// service-user/admin admission with separate host/worker/game identities.
#[derive(Clone, Copy)]
pub struct LocalPrivilege(());
pub fn audit_setup_rotation(
    c: &rusqlite::Connection,
    _local: LocalPrivilege,
    now: i64,
) -> crate::Result<()> {
    super::state::changed(c, false, "setup_code_rotate", "local_control", "setup", now)?;
    Ok(())
}
impl LocalPrivilege {
    /// Call only after protected local socket/pipe admission and peer checks.
    pub fn from_privileged_local_channel() -> Self {
        Self(())
    }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RecoveryRequest {
    pub request_id: Uuid,
    pub account_id: Uuid,
    pub device_key: String,
    pub device_name: String,
    pub revoke_all: bool,
}
pub fn recover(
    state: &HostState,
    _local: LocalPrivilege,
    request: &RecoveryRequest,
) -> Result<Device> {
    let key = decode::<32>(&request.device_key)?;
    let hash = hex::encode(Sha256::digest(canonical(
        &serde_json::to_value(request).map_err(|_| PAIRING)?,
    )));
    state.write(|c| {
        schema(c)?;
        if let Some(id) = receipt(c, "recovery", &request.request_id.to_string(), &hash)? {
            return super::owners::active_device_in(c, id)?
                .ok_or_else(|| super::state::reject(crate::IdentityErrorCode::Forbidden));
        }
        let old = super::state::snapshot_in(c)?.primary_account_id
            .ok_or_else(|| super::state::reject(crate::IdentityErrorCode::Conflict))?;
        // Recovery replaces a key offline but cannot invent a Java account.
        // A new account must first complete online enrollment.
        if !super::state::account_exists(c, request.account_id)? {
            return Err(super::state::reject(crate::IdentityErrorCode::Forbidden));
        }
        let device = HostState::enroll_device_in(
            c, request.account_id, &request.device_name, key, state.now_ms(),
        )?;
        c.execute(
            "DELETE FROM machine_owners WHERE account_id=?1 OR account_id=?2",
            params![old.to_string(), request.account_id.to_string()],
        )?;
        c.execute("UPDATE machine_host SET primary_account_id=?1 WHERE singleton=1",
            [request.account_id.to_string()])?;
        if request.revoke_all {
            c.execute(
                "UPDATE machine_devices SET revoked_at_ms=?1 WHERE device_id!=?2 AND revoked_at_ms IS NULL",
                params![state.now_ms(), device.device_id.to_string()],
            )?;
        }
        super::state::changed(c, true, "local_recovery", "local_control",
            &request.account_id.to_string(), state.now_ms())?;
        save_receipt(c, "recovery", &request.request_id.to_string(), &hash, device.device_id)?;
        Ok(device)
    }).map_err(|_| PAIRING)
}
