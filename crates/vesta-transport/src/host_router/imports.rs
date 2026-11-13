//! Owner-authenticated import extension point. H16 adds jobs here.
use super::*;
use vesta_control::machine::{imports as domain, resources};
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Resources {
    game_port: Option<u16>,
    memory_mi_b: Option<u64>,
}
pub(super) fn dispatch(
    router: &HostRouter,
    device: &Device,
    method: &Method,
    path: &str,
    bytes: &[u8],
) -> Reply {
    let binding = OwnerBinding {
        host_id: router
            .identity
            .state
            .snapshot()
            .unwrap()
            .host_id
            .to_string(),
        account_id: device.account_id.to_string(),
        device_id: device.device_id.to_string(),
    };
    macro_rules! input {
        () => {
            match parse(bytes) {
                Ok(v) => v,
                Err(e) => return e,
            }
        };
    }
    macro_rules! result {
        ($v:expr) => {
            match $v {
                Ok(v) => routes::json(serde_json::to_value(v).unwrap()),
                Err(e) => failure(StatusCode::BAD_REQUEST, e.code()),
            }
        };
    }
    match (method.as_str(), path) {
        ("GET", "/v1/host/folders/roots") => result!(router.folders.roots()),
        ("POST", "/v1/host/folders/resolve") => {
            let r: Resolve = input!();
            result!(router.folders.resolve(
                &binding,
                &domain::ResolveRequest {
                    root_id: r.root_id,
                    relative_path: r.relative_path
                }
            ))
        }
        ("POST", "/v1/host/folders/list") => {
            let r: List = input!();
            result!(router.folders.list(
                &binding,
                &domain::ListRequest {
                    folder_token: r.folder_token,
                    cursor: r.cursor
                }
            ))
        }
        ("GET", "/v1/host/imports/candidates") => result!(domain::amp::candidates(
            &router.folders,
            &binding,
            &domain::CandidateScan::default()
        )),
        ("POST", "/v1/host/imports/inspect") => {
            let r: Inspect = input!();
            result!(domain::inspection::inspect(
                &router.folders,
                &binding,
                &domain::InspectRequest {
                    folder_token: r.folder_token,
                    original_path: None,
                    destination: None,
                    game_port: r.game_port
                }
            ))
        }
        ("POST", "/v1/host/imports/resources") => {
            let request: Resources = input!();
            let mut reservations = resources::Reservations::default();
            if let Ok(entries) = router.identity.state.visible_servers(device.account_id) {
                for (e, _) in entries {
                    reservations.ports.insert(e.game_port);
                }
            }
            // No heap reservation catalogue exists yet: refuse to guess remaining RAM.
            let memory = if reservations.ports.is_empty() {
                resources::plan_host_memory(&reservations, request.memory_mi_b)
            } else {
                resources::plan_memory(None, &reservations, request.memory_mi_b)
            };
            routes::json(
                json!({"memory":memory,"port":resources::plan_port(&reservations,request.game_port)}),
            )
        }
        // TODO(H16): add plan/apply/status/cancel handlers without changing admission.
        (_, "/v1/host/imports/plan" | "/v1/host/imports/apply") => {
            failure(StatusCode::NOT_IMPLEMENTED, "import_jobs_not_implemented")
        }
        _ if path.starts_with("/v1/host/imports/jobs/") => {
            failure(StatusCode::NOT_IMPLEMENTED, "import_jobs_not_implemented")
        }
        _ => failure(StatusCode::NOT_FOUND, "not_found"),
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Resolve {
    root_id: String,
    relative_path: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct List {
    folder_token: String,
    cursor: Option<String>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Inspect {
    folder_token: String,
    game_port: Option<u16>,
}
