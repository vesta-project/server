//! Fresh signed owner changes. Challenge storage is process-local and single use.
use super::*;
use vesta_control::machine::sessions::signed_bytes;
#[derive(Default)]
pub(super) struct Challenges(
    Mutex<std::collections::HashMap<Uuid, (Uuid, std::time::Instant, String)>>,
);
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Change {
    target_account_id: Uuid,
    nonce: Uuid,
    expires_at: String,
    revision: String,
    signature: String,
    role: Option<String>,
}
impl Challenges {
    pub fn dispatch(
        &self,
        router: &HostRouter,
        device: &Device,
        method: &Method,
        path: &str,
        headers: &http::HeaderMap,
        bytes: &[u8],
    ) -> Option<Reply> {
        if path != "/v1/host/ownership/challenge"
            && path != "/v1/host/ownership/transfer"
            && !path.starts_with("/v1/host/owners/")
        {
            return None;
        }
        let state = &router.identity.state;
        if state.host_role(device.account_id).ok().flatten()
            != Some(vesta_control::machine::owners::HostRole::PrimaryOwner)
        {
            return Some(failure(StatusCode::FORBIDDEN, "forbidden"));
        }
        if path == "/v1/host/ownership/challenge" && *method == Method::POST {
            let nonce = Uuid::new_v4();
            let expires = (chrono::DateTime::from_timestamp_millis(state.now_ms()).unwrap()
                + chrono::Duration::seconds(60))
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
            let mut pending = self.0.lock().unwrap();
            pending.retain(|_, (_, until, _)| *until > std::time::Instant::now());
            if pending.len() >= 1024 {
                return Some(failure(StatusCode::SERVICE_UNAVAILABLE, "host_busy"));
            }
            pending.insert(
                nonce,
                (
                    device.device_id,
                    std::time::Instant::now() + Duration::from_secs(60),
                    expires.clone(),
                ),
            );
            return Some(routes::json(json!({"nonce":nonce,"expiresAt":expires})));
        }
        let transfer = path == "/v1/host/ownership/transfer";
        if transfer && *method != Method::POST
            || !transfer && !matches!(*method, Method::PUT | Method::DELETE)
        {
            return Some(failure(
                StatusCode::METHOD_NOT_ALLOWED,
                "method_not_allowed",
            ));
        }
        let request: Change = match parse(bytes) {
            Ok(r) => r,
            Err(e) => return Some(e),
        };
        let pending = self.0.lock().unwrap().remove(&request.nonce);
        if !pending.is_some_and(|(d, t, e)| {
            d == device.device_id && t > std::time::Instant::now() && e == request.expires_at
        }) {
            return Some(failure(StatusCode::FORBIDDEN, "confirmation_invalid"));
        }
        let action = if transfer {
            "transfer"
        } else if *method == Method::PUT {
            "promote"
        } else {
            "demote"
        };
        if !transfer
            && (path.trim_start_matches("/v1/host/owners/")
                != request.target_account_id.to_string()
                || *method == Method::PUT && request.role.as_deref() != Some("owner"))
        {
            return Some(failure(StatusCode::BAD_REQUEST, "invalid_request"));
        }
        let revision = match request.revision.parse::<u64>() {
            Ok(v) => v,
            Err(_) => return Some(failure(StatusCode::BAD_REQUEST, "invalid_request")),
        };
        if headers
            .get("if-match")
            .and_then(|v| v.to_str().ok())
            .map(|v| v.trim_matches('"'))
            != Some(request.revision.as_str())
        {
            return Some(failure(
                StatusCode::PRECONDITION_FAILED,
                "revision_mismatch",
            ));
        }
        let snapshot = state.snapshot().ok()?;
        let payload = json!({"hostId":snapshot.host_id,"deviceId":device.device_id,"action":action,"targetAccountId":request.target_account_id,"nonce":request.nonce,"expiresAt":request.expires_at,"revision":request.revision});
        let signature = match vesta_host_client::decode::<64>(&request.signature) {
            Ok(v) => v,
            Err(_) => return Some(failure(StatusCode::FORBIDDEN, "confirmation_invalid")),
        };
        if ring::signature::UnparsedPublicKey::new(&ring::signature::ED25519, device.public_key)
            .verify(&signed_bytes("vesta.host.owner.v1", &payload), &signature)
            .is_err()
        {
            return Some(failure(StatusCode::FORBIDDEN, "confirmation_invalid"));
        }
        let result = if transfer {
            state.transfer_ownership(
                Actor::Account(device.account_id),
                request.target_account_id,
                revision,
            )
        } else {
            state.set_owner(
                Actor::Account(device.account_id),
                request.target_account_id,
                *method == Method::PUT,
                revision,
            )
        };
        Some(match result {
            Ok(s) => routes::json(
                json!({"authGeneration":s.auth_generation.to_string(),"revision":s.revision.to_string()}),
            ),
            Err(_) => failure(StatusCode::CONFLICT, "ownership_conflict"),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ring::signature::{Ed25519KeyPair, KeyPair};
    fn json_body(reply: Reply) -> serde_json::Value {
        let ReplyBody::Bytes(bytes) = reply.into_body() else {
            panic!("JSON")
        };
        serde_json::from_slice(&bytes).unwrap()
    }
    fn change(
        challenges: &Challenges,
        router: &HostRouter,
        device: &Device,
        key: &Ed25519KeyPair,
        target: Uuid,
        action: &str,
    ) -> Reply {
        let challenge = json_body(
            challenges
                .dispatch(
                    router,
                    device,
                    &Method::POST,
                    "/v1/host/ownership/challenge",
                    &http::HeaderMap::new(),
                    b"",
                )
                .unwrap(),
        );
        let state = &router.identity.state;
        let snap = state.snapshot().unwrap();
        let revision = snap.revision.to_string();
        let signed = json!({"hostId":snap.host_id,"deviceId":device.device_id,"action":action,"targetAccountId":target,"nonce":challenge["nonce"],"expiresAt":challenge["expiresAt"],"revision":revision});
        let signature = vesta_host_client::encode(
            key.sign(&vesta_host_client::requests::signing_bytes(
                "vesta.host.owner.v1",
                &signed,
            ))
            .as_ref(),
        );
        let bytes=serde_json::to_vec(&json!({"targetAccountId":target,"nonce":challenge["nonce"],"expiresAt":challenge["expiresAt"],"revision":revision,"signature":signature,"role":if action=="promote"{Some("owner")}else{None}})).unwrap();
        let mut headers = http::HeaderMap::new();
        headers.insert("if-match", format!("\"{revision}\"").parse().unwrap());
        let path = if action == "transfer" {
            "/v1/host/ownership/transfer".to_owned()
        } else {
            format!("/v1/host/owners/{target}")
        };
        let method = match action {
            "transfer" => Method::POST,
            "promote" => Method::PUT,
            _ => Method::DELETE,
        };
        let reply = challenges
            .dispatch(router, device, &method, &path, &headers, &bytes)
            .unwrap();
        assert_eq!(
            challenges
                .dispatch(router, device, &method, &path, &headers, &bytes)
                .unwrap()
                .status(),
            StatusCode::FORBIDDEN,
            "confirmation replay"
        );
        reply
    }
    #[test]
    fn signed_owner_changes_transfer_and_primary_protection() {
        let dir = tempfile::tempdir().unwrap();
        let state =
            vesta_control::machine::HostState::open(dir.path(), Arc::new(vesta_state::SystemClock))
                .unwrap();
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        let key_a = Ed25519KeyPair::from_seed_unchecked(&[21; 32]).unwrap();
        let key_b = Ed25519KeyPair::from_seed_unchecked(&[22; 32]).unwrap();
        let device_a = state
            .claim(a, "A", key_a.public_key().as_ref().try_into().unwrap())
            .unwrap();
        let device_b = state
            .write(|c| {
                vesta_control::machine::HostState::enroll_device_in(
                    c,
                    b,
                    "B",
                    key_b.public_key().as_ref().try_into().unwrap(),
                    state.now_ms(),
                )
            })
            .unwrap();
        let router = HostRouter::new(
            Arc::new(HostIdentity::new(state.clone()).unwrap()),
            "Fixture".into(),
            Arc::new(FolderBrowser::new(vec![], vec![dir.path().to_owned()]).unwrap()),
        );
        let challenges = Challenges::default();
        assert_eq!(
            change(&challenges, &router, &device_a, &key_a, b, "promote").status(),
            StatusCode::OK
        );
        assert_eq!(
            challenges
                .dispatch(
                    &router,
                    &device_b,
                    &Method::POST,
                    "/v1/host/ownership/challenge",
                    &http::HeaderMap::new(),
                    b""
                )
                .unwrap()
                .status(),
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            change(&challenges, &router, &device_a, &key_a, b, "transfer").status(),
            StatusCode::OK
        );
        assert_eq!(state.snapshot().unwrap().primary_account_id, Some(b));
        assert_eq!(
            change(&challenges, &router, &device_b, &key_b, a, "transfer").status(),
            StatusCode::OK
        );
        assert_eq!(
            change(&challenges, &router, &device_a, &key_a, b, "demote").status(),
            StatusCode::OK
        );
        assert!(state.host_role(b).unwrap().is_none());
    }
}
