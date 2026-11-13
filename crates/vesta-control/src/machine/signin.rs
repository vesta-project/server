//! Account sign-in on a new device. Authority follows the verified Minecraft
//! account: a fresh account proof bound to this host's TLS fingerprint plus
//! device-key possession enrolls a device of an account that already holds a
//! role, with no link code or owner approval (unless the owner chose approval).
use super::{
    HostState,
    accounts::{AccountPurpose, AccountScope, HostAccounts},
    owners::{
        Device, device_by_key_in, enroll_device_in, holds_any_role_in, record_account_name_in,
    },
    settings::{NewDeviceSignIn, device_name, sign_in_mode_in},
    sessions::*,
    state::{changed, reject},
};
use serde::{Deserialize, Serialize};
use serde_json::json;

pub const NO_ACCESS: IdentityFailure = IdentityFailure("account_has_no_access");
pub const APPROVAL_REQUIRED: IdentityFailure = IdentityFailure("device_approval_required");
pub const KEY_UNUSABLE: IdentityFailure = IdentityFailure("device_key_unusable");

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SignInRequest {
    pub account_proof_id: String,
    pub device_key: String,
    pub device_name: String,
    pub signature: String,
}
/// A committed sign-in. Only [`sign_in`] constructs it; `HostSessions::signed_in`
/// turns it into the device's first bearer.
pub struct SignedIn {
    pub(crate) device: Device,
    /// False when this key was already an active device of the same account.
    pub created: bool,
}
impl SignedIn {
    pub fn device(&self) -> &Device {
        &self.device
    }
}
/// `fingerprint` is the accepted TLS snapshot fingerprint of the connection the
/// request arrived on; `scope` is built by the transport from that connection.
pub fn sign_in(
    state: &HostState,
    accounts: &mut HostAccounts,
    scope: &AccountScope,
    fingerprint: &str,
    request: &SignInRequest,
) -> Result<SignedIn> {
    if scope.purpose != AccountPurpose::Signin
        || scope.binding != fingerprint
        || scope.device_key != request.device_key
    {
        return Err(PAIRING);
    }
    let name = device_name(&request.device_name).ok_or(PAIRING)?;
    let key = decode::<32>(&request.device_key)?;
    let host = state.snapshot().map_err(|_| UNAVAILABLE)?.host_id;
    verify(
        &request.device_key,
        &request.signature,
        "vesta.host.signin.v1",
        &json!({"hostId":host,"fingerprint":fingerprint,"accountProofId":request.account_proof_id,"deviceKey":request.device_key,"deviceName":request.device_name}),
    )?;
    // Every definitive answer consumes the proof; only transient failures keep it.
    accounts
        .consume_checked(
            &request.account_proof_id,
            scope,
            |account, verified_name, proof_live| {
                state
                    .write(|c| {
                        if !proof_live() {
                            return Err(reject(crate::IdentityErrorCode::CodeInvalid));
                        }
                        if !holds_any_role_in(c, account)? {
                            return Ok(Err(NO_ACCESS));
                        }
                        let now = state.now_ms();
                        if let Some(existing) = device_by_key_in(c, &key)? {
                            if existing.account_id != account || existing.revoked_at_ms.is_some() {
                                return Ok(Err(KEY_UNUSABLE));
                            }
                            record_account_name_in(c, account, verified_name, now)?;
                            return Ok(Ok(SignedIn {
                                device: existing,
                                created: false,
                            }));
                        }
                        if sign_in_mode_in(c)? == NewDeviceSignIn::Approval {
                            return Ok(Err(APPROVAL_REQUIRED));
                        }
                        let device = enroll_device_in(c, account, &name, key, now)?;
                        record_account_name_in(c, account, verified_name, now)?;
                        changed(
                            c,
                            true,
                            "device_sign_in",
                            &account.to_string(),
                            &device.device_id.to_string(),
                            now,
                        )?;
                        Ok(Ok(SignedIn {
                            device,
                            created: true,
                        }))
                    })
                    .map_err(|_| PAIRING)
            },
        )?
}
