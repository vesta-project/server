//! Signed link requests; code possession alone never enrolls a device.
use super::{
    HostState,
    accounts::{AccountPurpose, AccountScope, HostAccounts},
    claim::{receipt, save_receipt, schema},
    owners::{Actor, Device, active_device_in, require_owner},
    sessions::*,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LinkCodeRequest {
    pub target_account_id: Uuid,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LinkApprovalRequest {
    pub request_hash: String,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SignedLinkRequest {
    pub host_id: Uuid,
    pub link_id: Uuid,
    pub device_key: String,
    pub account_id: Uuid,
    pub requested_role: String,
    pub device_name: String,
    pub nonce: String,
    pub expires_at: String,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LinkRequest {
    pub exchange_id: String,
    pub account_proof_id: String,
    pub request: SignedLinkRequest,
    pub signature: String,
}
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LinkStatus {
    #[serde(skip_serializing)]
    pub link_id: Uuid,
    pub state: String,
    pub request_hash: Option<String>,
    pub device_name: Option<String>,
    pub account_id: Option<Uuid>,
    pub expires_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device_id: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request: Option<SignedLinkRequest>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LinkSubmitted {
    pub link_id: Uuid,
    pub state: String,
    pub request_hash: String,
}
#[derive(Serialize)]
#[serde(untagged)]
pub enum LinkPollResponse {
    Approved {
        state: String,
        #[serde(rename = "deviceId")]
        device_id: Uuid,
    },
    Pending(Box<LinkStatus>),
}
struct Pending {
    request: SignedLinkRequest,
    signature: String,
    hash: String,
    deadline: Deadline,
}
pub struct HostLinks {
    state: HostState,
    active: Option<Link>,
}
struct Link {
    id: Uuid,
    target: Option<Uuid>,
    issuer: Option<Uuid>,
    deadline: Deadline,
    pending: Option<Pending>,
    approved: Option<Uuid>,
}
/// An OS-authenticated local caller or an authenticated owner device. Never
/// deserialize this from network input. Device authority is rechecked in the DB.
#[derive(Clone, Copy)]
pub enum LinkApprover {
    Device(Uuid),
    Local(super::recovery::LocalPrivilege),
}
impl HostLinks {
    pub fn new(state: HostState) -> Self {
        Self {
            state,
            active: None,
        }
    }
    pub fn create(&mut self, actor: LinkApprover, target: Option<Uuid>) -> Result<Uuid> {
        if self
            .state
            .snapshot()
            .map_err(|_| UNAVAILABLE)?
            .primary_account_id
            .is_none()
        {
            return Err(PAIRING);
        }
        self.state
            .write(|c| {
                authorize(c, actor)?;
                super::state::changed(
                    c,
                    false,
                    "link_code_rotate",
                    &audit(actor),
                    "link",
                    self.state.now_ms(),
                )?;
                Ok(())
            })
            .map_err(|_| PAIRING)?;
        let id = Uuid::new_v4();
        self.active = Some(Link {
            id,
            target,
            issuer: match actor {
                LinkApprover::Device(d) => Some(d),
                _ => None,
            },
            deadline: Deadline::new(self.state.now_ms(), 600),
            pending: None,
            approved: None,
        });
        Ok(id)
    }
    pub fn submit(
        &mut self,
        accounts: &mut HostAccounts,
        scope: &AccountScope,
        request: LinkRequest,
    ) -> Result<LinkStatus> {
        let link = self.active.as_mut().ok_or(PAIRING)?;
        let now = self.state.now_ms();
        if !link.deadline.live(now)
            || link.approved.is_some()
            || request.request.link_id != link.id
            || scope.purpose != AccountPurpose::Link
            || scope.binding != request.exchange_id
            || scope.device_key != request.request.device_key
            || request.request.host_id != self.state.snapshot().map_err(|_| UNAVAILABLE)?.host_id
            || request.request.requested_role != "linked_device"
            || super::settings::device_name(&request.request.device_name).as_deref()
                != Some(request.request.device_name.as_str())
            || link.target.is_some_and(|a| a != request.request.account_id)
        {
            return Err(PAIRING);
        }
        decode::<32>(&request.request.nonce)?;
        decode::<32>(&request.request.device_key)?;
        let expires = chrono::DateTime::parse_from_rfc3339(&request.request.expires_at)
            .map_err(|_| PAIRING)?
            .timestamp_millis();
        if expires <= now || expires > now.saturating_add(120_000) {
            return Err(PAIRING);
        }
        let value = serde_json::to_value(&request.request).map_err(|_| PAIRING)?;
        verify(
            &scope.device_key,
            &request.signature,
            "vesta.host.link.v1",
            &value,
        )?;
        let hash = hex::encode(Sha256::digest(canonical(&value)));
        if let Some(p) = &link.pending {
            if p.hash != hash || p.signature != request.signature {
                return Err(PAIRING);
            }
            return Ok(status(link, false));
        }
        accounts.consume(&request.account_proof_id, scope, |account| {
            if account != request.request.account_id {
                return Err(PAIRING);
            }
            Ok(())
        })?;
        // Exact expiry is enforced independently of code lifetime and monotonic time.
        link.pending = Some(Pending {
            request: request.request,
            signature: request.signature,
            hash,
            deadline: Deadline::until(now, expires),
        });
        Ok(status(link, false))
    }
    pub fn status(&self, id: Uuid, owner: bool) -> Result<LinkStatus> {
        let l = self.active.as_ref().filter(|l| l.id == id).ok_or(PAIRING)?;
        if !l.deadline.live(self.state.now_ms())
            || (l.approved.is_none()
                && l.pending
                    .as_ref()
                    .is_some_and(|p| !p.deadline.live(self.state.now_ms())))
        {
            return Err(PAIRING);
        }
        Ok(status(l, owner))
    }
    pub fn current_status(&self) -> Result<LinkStatus> {
        self.status(self.active.as_ref().ok_or(PAIRING)?.id, true)
    }
    pub fn approve(&mut self, actor: LinkApprover, id: Uuid, hash: &str) -> Result<Device> {
        self.approve_checked(actor, id, hash, || true)
    }
    pub fn approve_checked(
        &mut self,
        actor: LinkApprover,
        id: Uuid,
        hash: &str,
        capability_live: impl Fn() -> bool,
    ) -> Result<Device> {
        let link = self.active.as_mut().filter(|l| l.id == id).ok_or(PAIRING)?;
        let p = link.pending.as_ref().ok_or(PAIRING)?;
        let now = self.state.now_ms();
        if !link.deadline.live(now) || !p.deadline.live(now) || p.hash != hash {
            return Err(PAIRING);
        }
        let device = self
            .state
            .write(|c| {
                if !capability_live()
                    || !link.deadline.live(self.state.now_ms())
                    || !p.deadline.live(self.state.now_ms())
                {
                    return Err(super::state::reject(crate::IdentityErrorCode::CodeInvalid));
                }
                authorize(c, actor)?;
                if let Some(issuer) = link.issuer {
                    let d = active_device_in(c, issuer)?
                        .ok_or_else(|| super::state::reject(crate::IdentityErrorCode::Forbidden))?;
                    require_owner(c, Actor::Account(d.account_id))?;
                }
                schema(c)?;
                if let Some(device) = receipt(c, "link", &id.to_string(), hash)? {
                    return active_device_in(c, device)?
                        .ok_or_else(|| super::state::reject(crate::IdentityErrorCode::Forbidden));
                }
                let key = decode::<32>(&p.request.device_key)
                    .map_err(|_| super::state::reject(crate::IdentityErrorCode::InvalidRequest))?;
                let device = HostState::enroll_device_in(
                    c,
                    p.request.account_id,
                    &p.request.device_name,
                    key,
                    now,
                )?;
                save_receipt(c, "link", &id.to_string(), hash, device.device_id)?;
                Ok(device)
            })
            .map_err(|_| PAIRING)?;
        link.approved = Some(device.device_id);
        Ok(device)
    }
    pub fn invalidate(&mut self) {
        self.active = None;
    }
}
pub(crate) fn authorize(c: &rusqlite::Connection, actor: LinkApprover) -> crate::Result<()> {
    match actor {
        LinkApprover::Local(_) => Ok(()),
        LinkApprover::Device(id) => {
            let d = active_device_in(c, id)?
                .ok_or_else(|| super::state::reject(crate::IdentityErrorCode::Forbidden))?;
            require_owner(c, Actor::Account(d.account_id))
        }
    }
}
fn audit(actor: LinkApprover) -> String {
    match actor {
        LinkApprover::Local(_) => "local_control".into(),
        LinkApprover::Device(id) => id.to_string(),
    }
}
fn status(l: &Link, owner: bool) -> LinkStatus {
    LinkStatus {
        link_id: l.id,
        state: if l.approved.is_some() {
            "approved"
        } else if l.pending.is_some() {
            "awaiting_approval"
        } else {
            "awaiting_request"
        }
        .into(),
        request_hash: l.pending.as_ref().map(|p| p.hash.clone()),
        device_name: l.pending.as_ref().map(|p| p.request.device_name.clone()),
        account_id: l.pending.as_ref().map(|p| p.request.account_id),
        expires_at: l
            .pending
            .as_ref()
            .map(|p| p.deadline.text())
            .unwrap_or_else(|| l.deadline.text()),
        device_id: l.approved,
        request: if owner {
            l.pending.as_ref().map(|p| p.request.clone())
        } else {
            None
        },
        signature: if owner {
            l.pending.as_ref().map(|p| p.signature.clone())
        } else {
            None
        },
    }
}
