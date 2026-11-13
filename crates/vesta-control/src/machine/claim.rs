//! Atomic claim after the transport has authorized the current PAKE capability.
use super::{
    HostState,
    accounts::{AccountPurpose, AccountScope, HostAccounts},
    owners::Device,
    sessions::*,
};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::cell::RefCell;
#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ClaimRequest {
    pub exchange_id: String,
    pub account_proof_id: String,
    pub device_name: String,
    pub signature: String,
}
/// One verified enrollment callback. Transport's ClaimAuthority implementation
/// delegates here while holding the PAKE mutex, preventing code rotation between
/// authorization and commit. The account store remains locked through commit.
pub struct ClaimCommit<'a> {
    state: &'a HostState,
    accounts: RefCell<&'a mut HostAccounts>,
    scope: AccountScope,
    request: ClaimRequest,
    result: RefCell<Option<Device>>,
}
impl<'a> ClaimCommit<'a> {
    pub fn new(
        state: &'a HostState,
        accounts: &'a mut HostAccounts,
        scope: AccountScope,
        request: ClaimRequest,
    ) -> Self {
        Self {
            state,
            accounts: RefCell::new(accounts),
            scope,
            request,
            result: RefCell::new(None),
        }
    }
    pub fn is_unclaimed(&self) -> bool {
        self.state
            .snapshot()
            .is_ok_and(|s| s.primary_account_id.is_none())
    }
    pub fn commit(&self, exchange: &str, device_key: &str, is_claim: bool) -> Result<()> {
        self.commit_checked(exchange, device_key, is_claim, || true)
    }
    pub fn commit_checked(
        &self,
        exchange: &str,
        device_key: &str,
        is_claim: bool,
        capability_live: impl Fn() -> bool,
    ) -> Result<()> {
        if !is_claim
            || self.scope.purpose != AccountPurpose::Claim
            || self.scope.binding != exchange
            || self.request.exchange_id != exchange
            || self.scope.device_key != device_key
        {
            return Err(PAIRING);
        }
        let host = self.state.snapshot().map_err(|_| UNAVAILABLE)?.host_id;
        verify(
            device_key,
            &self.request.signature,
            "vesta.host.claim.v1",
            &json!({"hostId":host,"exchangeId":exchange,"accountProofId":self.request.account_proof_id,"deviceKey":device_key,"deviceName":self.request.device_name}),
        )?;
        let body = hex::encode(sha2::Sha256::digest(canonical(
            &serde_json::to_value(&self.request).map_err(|_| PAIRING)?,
        )));
        // Receipt and unique device key ensure lost replies never create a second
        // owner or device. Reconnection normally resumes with device session proof.
        let existing = self
            .state
            .write(|c| {
                schema(c)?;
                receipt(c, "claim", exchange, &body)
            })
            .map_err(|_| PAIRING)?;
        let device = if let Some(id) = existing {
            self.state
                .active_device(id)
                .map_err(|_| UNAVAILABLE)?
                .ok_or(PAIRING)?
        } else {
            self.accounts.borrow_mut().consume_checked(
                &self.request.account_proof_id,
                &self.scope,
                |account, proof_live| {
                    self.state
                        .write(|c| {
                            if !proof_live() || !capability_live() {
                                return Err(super::state::reject(
                                    crate::IdentityErrorCode::CodeInvalid,
                                ));
                            }
                            schema(c)?;
                            if let Some(id) = receipt(c, "claim", exchange, &body)? {
                                return super::owners::active_device_in(c, id)?.ok_or_else(|| {
                                    super::state::reject(crate::IdentityErrorCode::Conflict)
                                });
                            }
                            let device = HostState::claim_in(
                                c,
                                account,
                                &self.request.device_name,
                                decode::<32>(device_key).map_err(|_| {
                                    super::state::reject(crate::IdentityErrorCode::InvalidRequest)
                                })?,
                                self.state.now_ms(),
                            )?;
                            save_receipt(c, "claim", exchange, &body, device.device_id)?;
                            Ok(device)
                        })
                        .map_err(|_| PAIRING)
                },
            )?
        };
        if device.public_key != decode::<32>(device_key)? {
            return Err(PAIRING);
        }
        *self.result.borrow_mut() = Some(device);
        Ok(())
    }
    pub fn device(&self) -> Result<Device> {
        self.result.borrow().clone().ok_or(PAIRING)
    }
}
use sha2::Digest;
pub(crate) fn schema(c: &rusqlite::Connection) -> crate::Result<()> {
    c.execute_batch("CREATE TABLE IF NOT EXISTS machine_enrollment_receipts(kind TEXT NOT NULL,id TEXT NOT NULL,body_hash TEXT NOT NULL,device_id TEXT NOT NULL REFERENCES machine_devices(device_id),PRIMARY KEY(kind,id));")?;
    Ok(())
}
pub(crate) fn receipt(
    c: &rusqlite::Connection,
    kind: &str,
    id: &str,
    body: &str,
) -> crate::Result<Option<uuid::Uuid>> {
    let row: Option<(String, String)> = c
        .query_row(
            "SELECT body_hash,device_id FROM machine_enrollment_receipts WHERE kind=?1 AND id=?2",
            params![kind, id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    row.map(|(hash, device)| {
        if hash != body {
            return Err(super::state::reject(crate::IdentityErrorCode::Conflict));
        }
        super::state::uuid(device)
    })
    .transpose()
}
pub(crate) fn save_receipt(
    c: &rusqlite::Connection,
    kind: &str,
    id: &str,
    body: &str,
    device: uuid::Uuid,
) -> crate::Result<()> {
    c.execute(
        "INSERT INTO machine_enrollment_receipts(kind,id,body_hash,device_id) VALUES(?1,?2,?3,?4)",
        params![kind, id, body, device.to_string()],
    )?;
    Ok(())
}
