use super::access::{access_error, version};
use super::*;
use vesta_control::machine::roles::RoleFields;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Abilities {
    expected_version: String,
    capabilities: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Membership {
    expected_version: String,
    // Required key, nullable value (null removes the membership).
    role_id: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SavePreset {
    expected_version: String,
    server_id: Uuid,
    role_id: String,
    #[serde(default)]
    default_for_new_servers: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PatchPreset {
    expected_version: String,
    name: Option<String>,
    rank: Option<i32>,
    capabilities: Option<Vec<vesta_control::machine::roles::Grant>>,
    default_for_new_servers: Option<bool>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ApplyPreset {
    expected_version: String,
    server_id: Uuid,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Version {
    expected_version: String,
}

pub(super) fn dispatch(
    router: &HostRouter,
    actor: Actor,
    method: &Method,
    path: &str,
    query: Option<&str>,
    bytes: &[u8],
) -> Reply {
    if query.is_some() {
        return failure(StatusCode::BAD_REQUEST, "invalid_request");
    }
    let state = &router.identity.state;
    let malformed = || failure(StatusCode::NOT_FOUND, "not_found");
    let uuid = |id: &str| id.parse::<Uuid>().map_err(|_| malformed());
    let result = (|| {
        let parts: Vec<_> = path.split('/').collect();
        let result = match (method.as_str(), parts.as_slice()) {
            ("GET", ["capabilities"]) if bytes.is_empty() => {
                if !state
                    .has_host_capability(actor, vesta_control::identity::Capability::HostView)
                    .map_err(access_error)?
                {
                    return Err(failure(StatusCode::FORBIDDEN, "management_forbidden"));
                }
                return Ok(crate::manage::ok(access::catalog::capability_catalog()));
            }
            ("GET", ["people"]) if bytes.is_empty() => state.host_people(actor),
            ("PUT", ["people", account, "capabilities"]) => {
                let input: Abilities = parse(bytes)?;
                let value = state
                    .set_host_capabilities(
                        actor,
                        uuid(account)?,
                        version(&input.expected_version)?,
                        &input.capabilities,
                    )
                    .map_err(access_error)?;
                router.refresh_worker_policies(None)?;
                return Ok(crate::manage::ok(value));
            }
            ("GET", ["people", account, "memberships"]) if bytes.is_empty() => {
                state.host_memberships(actor, uuid(account)?)
            }
            ("PUT", ["people", account, "memberships", server]) => {
                let value: serde_json::Value = parse(bytes)?;
                if value.get("role_id").is_none() {
                    return Err(failure(StatusCode::BAD_REQUEST, "invalid_request"));
                }
                let input: Membership = parse(bytes)?;
                let server = uuid(server)?;
                let (snapshot, role) = state
                    .set_host_membership(
                        actor,
                        uuid(account)?,
                        server,
                        version(&input.expected_version)?,
                        input.role_id.as_deref(),
                    )
                    .map_err(access_error)?;
                router.refresh_worker_policies(Some(server))?;
                return Ok(crate::manage::ok(
                    json!({"version":snapshot.revision.to_string(),"server_id":server,"role_id":role.map(EffectiveRole::id)}),
                ));
            }
            ("GET", ["role-presets"]) if bytes.is_empty() => state.role_presets(actor),
            ("POST", ["role-presets"]) => {
                let input: SavePreset = parse(bytes)?;
                state.save_role_preset(
                    actor,
                    input.server_id,
                    &input.role_id,
                    version(&input.expected_version)?,
                    input.default_for_new_servers,
                )
            }
            ("PATCH", ["role-presets", id]) => {
                let input: PatchPreset = parse(bytes)?;
                if input.name.is_none()
                    && input.rank.is_none()
                    && input.capabilities.is_none()
                    && input.default_for_new_servers.is_none()
                {
                    return Err(failure(StatusCode::BAD_REQUEST, "invalid_request"));
                }
                state.patch_role_preset(
                    actor,
                    uuid(id)?,
                    version(&input.expected_version)?,
                    RoleFields {
                        name: input.name,
                        rank: input.rank,
                        capabilities: input.capabilities,
                    },
                    input.default_for_new_servers,
                )
            }
            ("DELETE", ["role-presets", id]) => {
                let input: Version = parse(bytes)?;
                state.delete_role_preset(actor, uuid(id)?, version(&input.expected_version)?)
            }
            ("POST", ["role-presets", id, "apply"]) => {
                let input: ApplyPreset = parse(bytes)?;
                let value = state
                    .apply_role_preset(
                        actor,
                        uuid(id)?,
                        input.server_id,
                        version(&input.expected_version)?,
                    )
                    .map_err(access_error)?;
                router.refresh_worker_policies(Some(input.server_id))?;
                return Ok(crate::manage::ok(value));
            }
            _ => return Err(malformed()),
        };
        Ok(crate::manage::ok(result.map_err(access_error)?))
    })();
    result.unwrap_or_else(|reply| reply)
}
