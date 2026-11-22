//! Server access is resolved by the host, never the worker identity database.
pub(super) mod catalog;
use super::*;
use catalog::capability_catalog;
use vesta_control::machine::roles::{Override, RoleFields};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RoleWrite {
    expected_version: String,
    name: Option<String>,
    rank: Option<i32>,
    capabilities: Option<Vec<vesta_control::machine::roles::Grant>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Version {
    expected_version: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Overrides {
    expected_version: String,
    overrides: Vec<Override>,
}

fn role_value(role: EffectiveRole) -> serde_json::Value {
    match role {
        EffectiveRole::Server(role) => role.summary(),
        EffectiveRole::EditedBuiltin { role, rank } => json!({"id":role.id(),"rank":rank}),
        EffectiveRole::Custom { id, rank } => json!({"id":id,"rank":rank}),
        EffectiveRole::PrimaryOwner | EffectiveRole::Owner => {
            let owner = vesta_control::identity::BuiltinRole::Owner;
            json!({"id":owner.id(),"name":owner.name(),"rank":owner.rank(),"builtin":true,
                "capabilities":vesta_control::worker::ServerRole::Owner.capabilities()
                    .map(|cap|json!({"capability":cap.id(),"scope":"*"})).collect::<Vec<_>>()})
        }
    }
}
fn user(
    id: Uuid,
    role: EffectiveRole,
    name: Option<String>,
    revision: u64,
    device_count: u64,
) -> serde_json::Value {
    let r = role_value(role);
    json!({"id":id,"display_name":name.clone().unwrap_or_else(||id.to_string()),"minecraft_accounts":[{"uuid":id,"name":name}],"roles":[{"role":r,"expires_at_ms":null}],"rank":r["rank"],"device_count":device_count,"machine_owner":matches!(role, EffectiveRole::PrimaryOwner | EffectiveRole::Owner),"version":revision.to_string()})
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Roles {
    add: Vec<String>,
    remove: Vec<String>,
    expected_version: String,
}
fn user_row(
    state: &vesta_control::machine::HostState,
    server: Uuid,
    roles: &[serde_json::Value],
    id: Uuid,
    role: EffectiveRole,
    name: Option<String>,
    revision: u64,
    count: u64,
) -> Result<serde_json::Value, Reply> {
    let mut row = user(id, role, name, revision, count);
    row["roles"][0]["role"] = roles
            .iter()
            .find(|value| value["id"] == role.id())
            .cloned()
            .ok_or_else(|| failure(StatusCode::SERVICE_UNAVAILABLE, "unavailable"))?;
    row["overrides"] = json!(
        state
            .capability_overrides(server, id)
            .map_err(access_error)?
    );
    Ok(row)
}

pub(super) fn version(value: &str) -> Result<u64, Reply> {
    value
        .parse::<u64>()
        .ok()
        .filter(|v| v.to_string() == value)
        .ok_or_else(|| failure(StatusCode::BAD_REQUEST, "invalid_request"))
}

fn refresh(router: &HostRouter, server: Uuid, device: &Device) -> Result<(), Reply> {
    let worker = router
        .workers
        .lock()
        .map_err(|_| failure(StatusCode::SERVICE_UNAVAILABLE, "worker_unavailable"))?
        .iter()
        .find(|w| w.server_id() == server)
        .cloned();
    if let Some(worker) = worker {
        subject(router, worker.as_ref(), device)?;
    }
    Ok(())
}

pub(super) fn dispatch(
    router: &HostRouter,
    device: &Device,
    server: Uuid,
    method: &Method,
    suffix: &str,
    query: Option<&str>,
    bytes: &[u8],
    fingerprint: &str,
) -> Option<Reply> {
    let path = suffix.strip_prefix("manage/")?;
    let kind = path.split('/').next()?;
    if !matches!(kind, "users" | "roles" | "devices" | "invitations") {
        return None;
    }
    let state = &router.identity.state;
    let actor = Actor::Account(device.account_id);
    if *method == Method::GET && query.is_none() && bytes.is_empty() {
        let result = if path == "roles" {
            Some(state.server_role_list(actor, server))
        } else if path == "roles/capabilities" {
            Some(state
                .check_server_access_view(actor, server)
                .map(|()| capability_catalog()))
        } else {
            None
        };
        if let Some(result) = result {
            return Some(result.map_or_else(access_error, crate::manage::ok));
        }
    }
    let rows = match state.server_access(actor, server) {
        Ok(r) => r,
        Err(e) => return Some(access_error(e)),
    };
    let counts = match state.active_device_counts() {
        Ok(v) => v,
        Err(e) => return Some(access_error(e)),
    };
    let count = |id: Uuid| counts.get(&id.to_string()).copied().unwrap_or(0);
    let revision = match state.snapshot() {
        Ok(s) => s.revision,
        Err(e) => return Some(access_error(e)),
    };
    let result = handle(
        router,
        device,
        server,
        method,
        path,
        query,
        bytes,
        fingerprint,
        rows,
        count,
        revision,
    );
    Some(result.unwrap_or_else(|error| error))
}

#[allow(clippy::too_many_arguments)]
fn handle(
    router: &HostRouter,
    device: &Device,
    server: Uuid,
    method: &Method,
    path: &str,
    query: Option<&str>,
    bytes: &[u8],
    fingerprint: &str,
    rows: Vec<(Uuid, EffectiveRole, Option<String>)>,
    count: impl Fn(Uuid) -> u32,
    revision: u64,
) -> Result<Reply, Reply> {
    let kind = path.split('/').next().unwrap_or_default();
    let state = &router.identity.state;
    let actor = Actor::Account(device.account_id);
    let result = if kind == "invitations" {
        use vesta_control::machine::invites;
        match (method.as_str(), path) {
            ("GET", "invitations") => match invites::list(state, actor, server) {
                Ok(v) => crate::manage::ok(v),
                Err(e) => access_error(e),
            },
            ("POST", "invitations") if query.is_none() => {
                let input: invites::CreateInvite = match parse(bytes) {
                    Ok(v) => v,
                    Err(e) => return Err(e),
                };
                match invites::issue(state, actor, server, fingerprint, input) {
                    Ok(v) => crate::manage::ok(v),
                    Err(e) => access_error(e),
                }
            }
            ("DELETE", _) if query.is_none() => {
                let id = path
                    .strip_prefix("invitations/")
                    .and_then(|s| Uuid::parse_str(s).ok());
                match id {
                    Some(id) => match invites::revoke(state, actor, server, id) {
                        Ok(()) => no_content(),
                        Err(e) => access_error(e),
                    },
                    None => failure(StatusCode::NOT_FOUND, "not_found"),
                }
            }
            _ => failure(StatusCode::NOT_FOUND, "not_found"),
        }
    } else if kind == "devices" || path.ends_with("/devices") {
        failure(StatusCode::NOT_FOUND, "not_supported")
    } else if kind == "roles" && query.is_none() {
        match (method.as_str(), path) {
            ("POST", _) if path.ends_with("/reset") => {
                let id = path.strip_prefix("roles/").and_then(|path| path.strip_suffix("/reset"))
                    .ok_or_else(|| failure(StatusCode::NOT_FOUND, "not_found"))?;
                let input: Version = parse(bytes)?;
                let (snapshot, role) = state.write_builtin_role(actor, server, id, version(&input.expected_version)?, None).map_err(access_error)?;
                refresh(router, server, device)?;
                crate::manage::ok(json!({"version":snapshot.revision.to_string(),"role":role}))
            }
            ("POST" | "PATCH", _) => {
                let id = if method == Method::POST && path == "roles" {
                    None
                } else if method == Method::PATCH {
                    let id = path.strip_prefix("roles/").filter(|id| !id.contains('/'))
                        .ok_or_else(|| failure(StatusCode::NOT_FOUND, "not_found"))?;
                    if let Ok(id) = id.parse::<Uuid>() {
                        Some(id)
                    } else {
                        let input: RoleWrite = parse(bytes)?;
                        let fields = RoleFields { name: input.name, rank: input.rank, capabilities: input.capabilities };
                        let (snapshot, role) = state.write_builtin_role(actor, server, id, version(&input.expected_version)?, Some(fields)).map_err(access_error)?;
                        refresh(router, server, device)?;
                        return Ok(crate::manage::ok(json!({"version":snapshot.revision.to_string(),"role":role})));
                    }
                } else {
                    return Err(failure(StatusCode::NOT_FOUND, "not_found"));
                };
                let input: RoleWrite = parse(bytes)?;
                let (snapshot, role) = state
                    .write_custom_role(
                        actor,
                        server,
                        id,
                        version(&input.expected_version)?,
                        RoleFields {
                            name: input.name,
                            rank: input.rank,
                            capabilities: input.capabilities,
                        },
                    )
                    .map_err(access_error)?;
                refresh(router, server, device)?;
                crate::manage::ok(json!({"version":snapshot.revision.to_string(),"role":role}))
            }
            ("DELETE", _) => {
                let id = custom_id(path)?;
                let input: Version = parse(bytes)?;
                let snapshot = state
                    .delete_custom_role(actor, server, id, version(&input.expected_version)?)
                    .map_err(access_error)?;
                refresh(router, server, device)?;
                crate::manage::ok(json!({"version":snapshot.revision.to_string(),"id":id}))
            }
            _ => failure(StatusCode::NOT_FOUND, "not_found"),
        }
    } else if method == Method::GET && path == "users" {
        let mut offset = 0usize;
        let mut limit = 100usize;
        if let Some(q) = query {
            for field in q.split('&') {
                let Some((key, value)) = field.split_once('=') else {
                    return Err(failure(StatusCode::BAD_REQUEST, "invalid_request"));
                };
                let Ok(n) = value.parse::<usize>() else {
                    return Err(failure(StatusCode::BAD_REQUEST, "invalid_request"));
                };
                match key {
                    "offset" if n <= 1000000 => offset = n,
                    "limit" if (1..=100).contains(&n) => limit = n,
                    _ => return Err(failure(StatusCode::BAD_REQUEST, "invalid_request")),
                }
            }
        }
        let next = (rows.len() > offset.saturating_add(limit)).then_some(offset + limit);
        let custom = state.server_roles(server).map_err(access_error)?;
        let users = rows
            .into_iter()
            .skip(offset)
            .take(limit)
            .map(|(id, r, n)| {
                user_row(
                    state,
                    server,
                    &custom,
                    id,
                    r,
                    n,
                    revision,
                    u64::from(count(id)),
                )
            })
            .collect::<Result<Vec<_>, _>>()?;
        crate::manage::ok(json!({"users":users,"next_offset":next}))
    } else if method == Method::PUT && query.is_none() && kind == "users" {
        let Some((target, action)) = path.strip_prefix("users/").and_then(|p| p.split_once('/'))
        else {
            return Err(failure(StatusCode::NOT_FOUND, "not_found"));
        };
        let target =
            Uuid::parse_str(target).map_err(|_| failure(StatusCode::NOT_FOUND, "not_found"))?;
        let (snapshot, role) = match action {
            "roles" => {
                let input: Roles = parse(bytes)?;
                if input.add.len() > 1 || input.remove.len() > 1 {
                    return Err(failure(StatusCode::BAD_REQUEST, "invalid_request"));
                }
                state
                    .assign_role(
                        actor,
                        server,
                        target,
                        input.add.first().map(String::as_str),
                        input.remove.first().map(String::as_str),
                        version(&input.expected_version)?,
                    )
                    .map_err(access_error)?
            }
            "overrides" => {
                let input: Overrides = parse(bytes)?;
                let (snapshot, role) = state
                    .set_capability_overrides(
                        actor,
                        server,
                        target,
                        version(&input.expected_version)?,
                        &input.overrides,
                    )
                    .map_err(access_error)?;
                (snapshot, Some(role))
            }
            _ => return Err(failure(StatusCode::NOT_FOUND, "not_found")),
        };
        refresh(router, server, device)?;
        let name = rows
            .into_iter()
            .find(|(id, _, _)| *id == target)
            .and_then(|(_, _, name)| name);
        let row = match role {
            Some(role) => user_row(
                state,
                server,
                &state.server_roles(server).map_err(access_error)?,
                target,
                role,
                name,
                snapshot.revision,
                u64::from(count(target)),
            )?,
            None => {
                json!({"id":target,"display_name":name.clone().unwrap_or_else(||target.to_string()),"minecraft_accounts":[{"uuid":target,"name":name}],"roles":[],"overrides":[],"rank":0,"device_count":count(target),"machine_owner":false,"version":snapshot.revision.to_string()})
            }
        };
        crate::manage::ok(row)
    } else {
        failure(StatusCode::NOT_FOUND, "not_supported")
    };
    Ok(result)
}

fn custom_id(path: &str) -> Result<Uuid, Reply> {
    let id = path.strip_prefix("roles/").unwrap_or_default();
    if id == "owner" || ServerRole::parse(id).is_ok() {
        return Err(failure(StatusCode::FORBIDDEN, "management_forbidden"));
    }
    Uuid::parse_str(id).map_err(|_| failure(StatusCode::NOT_FOUND, "not_found"))
}

pub(super) fn access_error(e: vesta_control::ControlError) -> Reply {
    use vesta_control::IdentityErrorCode as Code;
    match e {
        vesta_control::ControlError::Rejected { code, .. } => match code {
            Code::Forbidden | Code::LocalAuthorityRequired => {
                failure(StatusCode::FORBIDDEN, "management_forbidden")
            }
            Code::NotFound => failure(StatusCode::NOT_FOUND, "not_found"),
            Code::Conflict => failure(StatusCode::PRECONDITION_FAILED, "precondition_failed"),
            Code::RoleInUse => failure(StatusCode::CONFLICT, "role_in_use"),
            Code::InvalidRequest | Code::RoleInvalid => {
                failure(StatusCode::BAD_REQUEST, "invalid_request")
            }
            _ => failure(StatusCode::CONFLICT, "management_forbidden"),
        },
        _ => failure(StatusCode::SERVICE_UNAVAILABLE, "unavailable"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vesta_control::machine::{
        HostState,
        registry::{ServerEntry, ServerState, Visibility},
    };
    #[test]
    fn host_router_access_matrix_and_cross_server_isolation() {
        let dir = tempfile::tempdir().unwrap();
        let state =
            HostState::open(dir.path().join("host"), Arc::new(vesta_state::SystemClock)).unwrap();
        let owner = state.claim(Uuid::new_v4(), "Owner", [1; 32]).unwrap();
        let mut devices = vec![owner.clone()];
        for key in 2..=5 {
            devices.push(
                state
                    .write(|c| {
                        HostState::enroll_device_in(
                            c,
                            Uuid::new_v4(),
                            "Member",
                            [key; 32],
                            state.now_ms(),
                        )
                    })
                    .unwrap(),
            );
        }
        let mut servers = Vec::new();
        for port in [31001, 31002] {
            let id = Uuid::new_v4();
            state
                .register_server(
                    Actor::Account(owner.account_id),
                    &ServerEntry {
                        server_id: id,
                        name: format!("Server {port}"),
                        game_port: port,
                        state: ServerState::Stopped,
                        visibility: Visibility::Private,
                    },
                    state.snapshot().unwrap().revision,
                )
                .unwrap();
            servers.push(id);
        }
        for (i, role) in [
            (1, ServerRole::Admin),
            (2, ServerRole::Moderator),
            (3, ServerRole::PackUser),
        ] {
            state
                .set_server_role(
                    Actor::Account(owner.account_id),
                    servers[0],
                    devices[i].account_id,
                    Some(role),
                    state.snapshot().unwrap().revision,
                )
                .unwrap();
        }
        let router = HostRouter::new(
            Arc::new(HostIdentity::new(state.clone()).unwrap()),
            "Machine".into(),
            Arc::new(FolderBrowser::new(vec![], vec![dir.path().join("host")]).unwrap()),
        );
        let fp = "a".repeat(64);
        for (i, device) in devices.iter().enumerate() {
            for (method, path, body) in [
                (Method::GET, "manage/users", json!(null)),
                (Method::GET, "manage/roles", json!(null)),
                (Method::GET, "manage/invitations", json!(null)),
                (
                    Method::POST,
                    "manage/invitations",
                    json!({"role":"player","expires_minutes":60}),
                ),
            ] {
                let bytes = if body.is_null() {
                    Vec::new()
                } else {
                    serde_json::to_vec(&body).unwrap()
                };
                let reply = dispatch(
                    &router, device, servers[0], &method, path, None, &bytes, &fp,
                )
                .unwrap();
                assert_eq!(
                    reply.status(),
                    if i < 2 {
                        StatusCode::OK
                    } else {
                        StatusCode::FORBIDDEN
                    },
                    "{i} {path}"
                );
            }
            for path in ["manage/devices", "manage/users/invalid/devices"] {
                assert_eq!(
                    dispatch(
                        &router,
                        device,
                        servers[0],
                        &Method::GET,
                        path,
                        None,
                        &[],
                        &fp
                    )
                    .unwrap()
                    .status(),
                    if i < 2 {
                        StatusCode::NOT_FOUND
                    } else {
                        StatusCode::FORBIDDEN
                    }
                );
            }
        }
        for (i, device) in devices.iter().enumerate() {
            let input=serde_json::to_vec(&json!({"add":["player"],"remove":[],"expected_version":state.snapshot().unwrap().revision.to_string()})).unwrap();
            let path = format!("manage/users/{}/roles", devices[3].account_id);
            assert_eq!(
                dispatch(
                    &router,
                    device,
                    servers[0],
                    &Method::PUT,
                    &path,
                    None,
                    &input,
                    &fp
                )
                .unwrap()
                .status(),
                if i < 2 {
                    StatusCode::OK
                } else {
                    StatusCode::FORBIDDEN
                }
            );
            let issued = vesta_control::machine::invites::issue(
                &state,
                Actor::Account(owner.account_id),
                servers[0],
                &fp,
                vesta_control::machine::invites::CreateInvite {
                    role: "player".into(),
                    expires_minutes: 60,
                    multi_use: false,
                    max_uses: None,
                },
            )
            .unwrap();
            let path = format!("manage/invitations/{}", issued["id"].as_str().unwrap());
            assert_eq!(
                dispatch(
                    &router,
                    device,
                    servers[0],
                    &Method::DELETE,
                    &path,
                    None,
                    &[],
                    &fp
                )
                .unwrap()
                .status(),
                if i < 2 {
                    StatusCode::NO_CONTENT
                } else {
                    StatusCode::FORBIDDEN
                }
            );
        }
        assert!(
            state
                .server_access(Actor::Account(devices[1].account_id), servers[1])
                .is_err()
        );
        let rev = state.snapshot().unwrap().revision;
        assert!(
            state
                .set_server_role(
                    Actor::Account(devices[1].account_id),
                    servers[0],
                    devices[3].account_id,
                    Some(ServerRole::Admin),
                    rev
                )
                .is_err()
        );
        assert!(
            state
                .set_server_role(
                    Actor::Account(devices[1].account_id),
                    servers[0],
                    owner.account_id,
                    None,
                    rev
                )
                .is_err()
        );
        assert!(
            state
                .set_server_role(
                    Actor::Account(owner.account_id),
                    servers[0],
                    owner.account_id,
                    None,
                    rev
                )
                .is_err()
        );
        let before = state.snapshot().unwrap().auth_generation;
        state
            .set_server_role(
                Actor::Account(devices[1].account_id),
                servers[0],
                devices[3].account_id,
                None,
                rev,
            )
            .unwrap();
        assert!(state.snapshot().unwrap().auth_generation > before);
        assert_eq!(
            state
                .effective_role(devices[3].account_id, servers[0])
                .unwrap(),
            None
        );
    }
}

#[cfg(test)]
mod user_metadata_tests {
    use super::*;
    #[test]
    fn owner_rows_expose_only_aggregate_machine_device_metadata() {
        let id = Uuid::new_v4();
        let row = user(id, EffectiveRole::PrimaryOwner, Some("Owner".into()), 1, 2);
        assert_eq!(row["device_count"], 2);
        assert_eq!(row["machine_owner"], true);
        assert!(row.get("devices").is_none());
        let row = user(id, EffectiveRole::Server(ServerRole::Admin), None, 1, 3);
        assert_eq!(row["device_count"], 3);
        assert_eq!(row["machine_owner"], false);
    }
}
