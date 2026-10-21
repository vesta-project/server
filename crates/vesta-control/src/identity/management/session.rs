//! Informational projection only. Every effect still resolves current authority.
use super::*;
use crate::identity::{Capability, ResourceScope};
use crate::pack_access::PackAccessPolicy;
use serde_json::{Value, json};

impl ManagementAccess {
    pub fn session_projection(&self, token: &str, access: &PackAccessPolicy<'_>) -> Result<Value> {
        let subject = self.authenticate(token)?;
        let stores = self
            .stores
            .lock()
            .map_err(|_| ManagementError::Unavailable)?;
        let session = stores
            .sessions
            .get(&bound_hash(b"session", self.server_id(), token))
            .ok_or(ManagementError::SessionInvalid)?;
        self.write(|repo| {
            if session.expires <= repo.now || std::time::Instant::now() >= session.monotonic_expires
            {
                return Err(ManagementError::SessionInvalid);
            }
            let policy = repo.policy(Some(&subject))?;
            if !policy.has_current_identity() {
                return Err(ManagementError::SessionInvalid);
            }
            let (automatic, _) = self.automatic_roles(repo, &subject, access)?;
            let mut ids = policy
                .roles()
                .iter()
                .map(|id| id.as_str().to_owned())
                .collect::<Vec<_>>();
            ids.extend(automatic);
            ids.sort();
            ids.dedup();
            let roles = ids.iter().map(|id| {
                repo.connection().query_row("SELECT name,rank FROM roles WHERE id=?", [id], |r| {
                    Ok(json!({"id":id,"name":r.get::<_,String>(0)?,"rank":r.get::<_,i32>(1)?}))
                }).map_err(ManagementError::from)
            }).collect::<Result<Vec<_>>>()?;
            let capabilities = Capability::ALL
                .iter()
                .copied()
                .filter_map(|capability| {
                    let allowed = if access::safe(capability) {
                        self.pack_grant_in(repo, &subject, capability, access)
                            .map(|grant| grant.is_some())
                    } else if capability.scoped() {
                        // A flat string cannot describe resource scopes: advertise only
                        // capabilities held over all resources, excluding any overlap deny.
                        Ok(policy.holds_entirely(capability, &ResourceScope::ALL))
                    } else {
                        Ok(policy.check(capability, None).allowed())
                    };
                    match allowed {
                        Ok(true) => Some(Ok(capability.id())),
                        Ok(false) => None,
                        Err(e) => Some(Err(e)),
                    }
                })
                .collect::<Result<Vec<_>>>()?;
            Ok(
                json!({"server_id":self.server_id(),"principal_id":subject.principal.as_str(),
                "device_id":subject.device.as_str(),"roles":roles,"capabilities":capabilities,
                "session_expires_at_ms":session.expires}),
            )
        })
    }
}
