//! Server access is resolved by the host, never the worker identity database.
use super::*;
fn role_value(role: EffectiveRole) -> serde_json::Value {
    let (id, name, rank) = match role {
        EffectiveRole::PrimaryOwner | EffectiveRole::Owner => ("owner", "Owner", 100),
        EffectiveRole::Server(ServerRole::Admin) => ("administrator", "Administrator", 80),
        EffectiveRole::Server(ServerRole::Moderator) => ("moderator", "Moderator", 50),
        EffectiveRole::Server(ServerRole::Player) => ("player", "Player", 10),
    };
    json!({"id":id,"name":name,"rank":rank,"builtin":true,"capabilities":[]})
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
fn parse_role(id: &str) -> Option<ServerRole> {
    match id {
        "administrator" | "admin" => Some(ServerRole::Admin),
        "moderator" => Some(ServerRole::Moderator),
        "player" => Some(ServerRole::Player),
        _ => None,
    }
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
    if !matches!(
        kind,
        "users" | "roles" | "devices" | "invitations"
    ) {
        return None;
    }
    let state = &router.identity.state;
    let actor = Actor::Account(device.account_id);
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
                    Err(e) => return Some(e),
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
    } else if method == Method::GET && path == "roles" && query.is_none() {
        crate::manage::ok(
            json!({"roles":[role_value(EffectiveRole::Owner),role_value(EffectiveRole::Server(ServerRole::Admin)),role_value(EffectiveRole::Server(ServerRole::Moderator)),role_value(EffectiveRole::Server(ServerRole::Player))]}),
        )
    } else if method == Method::GET && path == "users" {
        let mut offset = 0usize;
        let mut limit = 100usize;
        if let Some(q) = query {
            for field in q.split('&') {
                let Some((key, value)) = field.split_once('=') else {
                    return Some(failure(StatusCode::BAD_REQUEST, "invalid_request"));
                };
                let Ok(n) = value.parse::<usize>() else {
                    return Some(failure(StatusCode::BAD_REQUEST, "invalid_request"));
                };
                match key {
                    "offset" if n <= 1000000 => offset = n,
                    "limit" if (1..=100).contains(&n) => limit = n,
                    _ => return Some(failure(StatusCode::BAD_REQUEST, "invalid_request")),
                }
            }
        }
        let next = (rows.len() > offset.saturating_add(limit)).then_some(offset + limit);
        crate::manage::ok(
            json!({"users":rows.into_iter().skip(offset).take(limit).map(|(id,r,n)|user(id,r,n,revision,u64::from(count(id)))).collect::<Vec<_>>(),"next_offset":next}),
        )
    } else if method == Method::PUT
        && path.starts_with("users/")
        && path.ends_with("/roles")
        && query.is_none()
    {
        let target = &path[6..path.len() - 6];
        let Ok(target) = Uuid::parse_str(target) else {
            return Some(failure(StatusCode::NOT_FOUND, "not_found"));
        };
        let input: Roles = match parse(bytes) {
            Ok(v) => v,
            Err(e) => return Some(e),
        };
        if input.expected_version != revision.to_string() {
            return Some(failure(
                StatusCode::PRECONDITION_FAILED,
                "precondition_failed",
            ));
        }
        if input.add.len() > 1
            || input.remove.len() > 1
            || input.remove.iter().any(|r| parse_role(r).is_none())
        {
            return Some(failure(StatusCode::BAD_REQUEST, "invalid_request"));
        }
        let role = match input.add.first() {
            Some(id) => match parse_role(id) {
                Some(r) => Some(r),
                None => return Some(failure(StatusCode::FORBIDDEN, "forbidden")),
            },
            None => None,
        };
        match state.set_server_role(actor, server, target, role, revision) {
            Ok(s) => {
                // Revoke the old policy in the worker before returning success.
                let worker = router
                    .workers
                    .lock()
                    .ok()
                    .and_then(|w| w.iter().find(|w| w.server_id() == server).cloned());
                if let Some(worker) = worker {
                    if subject(router, worker.as_ref(), device).is_err() {
                        return Some(failure(
                            StatusCode::SERVICE_UNAVAILABLE,
                            "worker_unavailable",
                        ));
                    }
                }
                let name = rows
                    .into_iter()
                    .find(|(id, _, _)| *id == target)
                    .and_then(|(_, _, n)| n);
                match role {
                    Some(r) => crate::manage::ok(user(
                        target,
                        EffectiveRole::Server(r),
                        name,
                        s.revision,
                        u64::from(count(target)),
                    )),
                    None => crate::manage::ok(
                        json!({"id":target,"display_name":name.unwrap_or_else(||target.to_string()),"minecraft_accounts":[{"uuid":target,"name":null}],"roles":[],"rank":0,"device_count":count(target),"machine_owner":false,"version":s.revision.to_string()}),
                    ),
                }
            }
            Err(e) => access_error(e),
        }
    } else {
        failure(StatusCode::NOT_FOUND, "not_supported")
    };
    Some(result)
}

fn access_error(e: vesta_control::ControlError) -> Reply {
    match e {
        vesta_control::ControlError::Rejected { code, .. } => failure(
            match code {
                vesta_control::IdentityErrorCode::Forbidden => StatusCode::FORBIDDEN,
                vesta_control::IdentityErrorCode::NotFound => StatusCode::NOT_FOUND,
                _ => StatusCode::CONFLICT,
            },
            "management_forbidden",
        ),
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
            (3, ServerRole::Player),
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
            for path in [
                "manage/devices",
                "manage/users/invalid/devices",
            ] {
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
        let id=Uuid::new_v4();
        let row=user(id,EffectiveRole::PrimaryOwner,Some("Owner".into()),1,2);
        assert_eq!(row["device_count"],2);assert_eq!(row["machine_owner"],true);
        assert!(row.get("devices").is_none());
        let row=user(id,EffectiveRole::Server(ServerRole::Admin),None,1,3);
        assert_eq!(row["device_count"],3);assert_eq!(row["machine_owner"],false);
    }
}
