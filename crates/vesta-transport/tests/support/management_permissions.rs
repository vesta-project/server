//! Real loopback TLS, owned temporary state, and exact permission wire shapes.
use super::*;
use vesta_control::identity::BuiltinRole;
fn send(
    f: &Fixture,
    method: &str,
    path: &str,
    body: &serde_json::Value,
    token: Option<&str>,
    key: Option<&str>,
) -> WireReply {
    let bytes = if body.is_null() {
        Vec::new()
    } else {
        serde_json::to_vec(body).unwrap()
    };
    let mut stream = f.stream();
    write!(stream,"{method} /api/v3/manage/{path} HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n",bytes.len()).unwrap();
    if let Some(token) = token {
        write!(stream, "Authorization: Bearer {token}\r\n").unwrap();
    }
    if let Some(key) = key {
        write!(stream, "Idempotency-Key: {key}\r\n").unwrap();
    }
    stream.write_all(b"\r\n").unwrap();
    stream.write_all(&bytes).unwrap();
    stream.flush().unwrap();
    read_reply(stream).unwrap()
}
fn actor(f: &Fixture, role: BuiltinRole) -> (String, String, String) {
    let (device, key) = enroll_management(f, role);
    let token = management_session(f, &device, &key);
    let id = f
        .host()
        .database()
        .read(|c| {
            c.query_row(
                "SELECT principal_id FROM devices WHERE id=?",
                [&device],
                |r| r.get::<_, String>(0),
            )
            .map_err(Into::into)
        })
        .unwrap();
    (id, device, token)
}
fn user(f: &Fixture, token: &str, id: &str) -> serde_json::Value {
    let r = send(
        f,
        "GET",
        &format!("users/{id}"),
        &serde_json::Value::Null,
        Some(token),
        None,
    );
    assert_eq!(r.status, 200);
    r.json()["data"].clone()
}
#[test]
fn permission_tls_users_roles_devices_and_pagination() {
    let f = Fixture::new("minecraft", 8, 16384, 5);
    let (id, device, token) = actor(&f, BuiltinRole::Owner);
    let (v, _, _) = actor(&f, BuiltinRole::Viewer);
    let result = user(&f, &token, &id);
    assert_eq!(result["rank"], 100);
    assert_eq!(result["device_count"], 1);
    assert!(result["minecraft_accounts"].is_array());
    assert!(result["version"].is_string());
    let page = send(
        &f,
        "GET",
        "users?limit=1",
        &serde_json::Value::Null,
        Some(&token),
        None,
    );
    assert_eq!(page.status, 200);
    assert_eq!(page.json()["data"]["users"].as_array().unwrap().len(), 1);
    assert_eq!(page.json()["data"]["next_offset"], 1);
    let r = send(
        &f,
        "GET",
        "roles",
        &serde_json::Value::Null,
        Some(&token),
        None,
    );
    assert_eq!(r.status, 200);
    assert!(
        r.json()["data"]["roles"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["id"] == "owner" && r["rank"] == 100)
    );
    let r = send(
        &f,
        "GET",
        &format!("users/{id}/devices"),
        &serde_json::Value::Null,
        Some(&token),
        None,
    );
    assert_eq!(r.status, 200);
    assert_eq!(r.json()["data"]["devices"][0]["id"], device);
    assert_eq!(
        send(
            &f,
            "GET",
            &format!("users/{v}/devices"),
            &serde_json::Value::Null,
            Some(&token),
            None
        )
        .status,
        200
    );
    for query in ["limit=101", "limit=1&limit=2", "token=secret", "offset=-1"] {
        management_reply_error(
            &send(
                &f,
                "GET",
                &format!("users?{query}"),
                &serde_json::Value::Null,
                Some(&token),
                None,
            ),
            400,
            "invalid_request",
        );
    }
}
#[test]
fn permission_tls_rank_self_top_role_and_versioned_idempotent_user_changes() {
    let f = Fixture::new("minecraft", 8, 16384, 5);
    let (owner, _, os) = actor(&f, BuiltinRole::Owner);
    let (admin, _, as_) = actor(&f, BuiltinRole::Administrator);
    let (equal, _, _) = actor(&f, BuiltinRole::Administrator);
    let (lower, _, _) = actor(&f, BuiltinRole::Viewer);
    for target in [&owner, &admin, &equal] {
        let v = user(&f, &as_, target);
        management_reply_error(
            &send(
                &f,
                "PUT",
                &format!("users/{target}/roles"),
                &serde_json::json!({"add":["moderator"],"remove":[],"expected_version":v["version"]}),
                Some(&as_),
                Some("tls-rank-refusal-0001"),
            ),
            403,
            "management_forbidden",
        );
    }
    let v = user(&f, &as_, &lower);
    let body = serde_json::json!({"add":["moderator"],"remove":[],"expected_version":v["version"]});
    let path = format!("users/{lower}/roles");
    let first = send(
        &f,
        "PUT",
        &path,
        &body,
        Some(&as_),
        Some("tls-lower-role-0001"),
    );
    assert_eq!(first.status, 200);
    assert_eq!(first.json()["data"]["rank"], 40);
    assert_eq!(
        send(
            &f,
            "PUT",
            &path,
            &body,
            Some(&as_),
            Some("tls-lower-role-0001")
        )
        .json()["data"],
        first.json()["data"]
    );
    management_reply_error(
        &send(
            &f,
            "PUT",
            &path,
            &body,
            Some(&as_),
            Some("tls-stale-role-0001"),
        ),
        412,
        "precondition_failed",
    );
    let v = user(&f, &os, &lower);
    management_reply_error(
        &send(
            &f,
            "PUT",
            &path,
            &serde_json::json!({"add":["owner"],"remove":[],"expected_version":v["version"]}),
            Some(&os),
            Some("tls-owner-role-0001"),
        ),
        403,
        "management_forbidden",
    );
}
#[test]
fn permission_tls_roles_create_patch_delete_and_capability_ceiling() {
    let f = Fixture::new("minecraft", 8, 16384, 5);
    let (_, _, token) = actor(&f, BuiltinRole::Administrator);
    let input = serde_json::json!({"name":"TLS custom","rank":30,"capabilities":[{"capability":"status.view","scope":"*"}]});
    let r = send(
        &f,
        "POST",
        "roles",
        &input,
        Some(&token),
        Some("tls-create-role-0001"),
    );
    assert_eq!(r.status, 200);
    let role = r.json()["data"].clone();
    let path = format!("roles/{}", role["id"].as_str().unwrap());
    management_reply_error(
        &send(
            &f,
            "PATCH",
            &path,
            &serde_json::json!({"expected_version":role["version"],"capabilities":[{"capability":"recovery.export","scope":"*"}]}),
            Some(&token),
            Some("tls-cap-ceiling-0001"),
        ),
        403,
        "management_forbidden",
    );
    assert_ne!(
        send(
            &f,
            "PATCH",
            &path,
            &serde_json::json!({"expected_version":role["version"],"rank":80}),
            Some(&token),
            Some("tls-rank-ceiling-0001")
        )
        .status,
        200
    );
    let r = send(
        &f,
        "PATCH",
        &path,
        &serde_json::json!({"expected_version":role["version"],"name":"TLS renamed"}),
        Some(&token),
        Some("tls-rename-role-0001"),
    );
    assert_eq!(r.status, 200);
    let body = serde_json::json!({"expected_version":r.json()["data"]["version"]});
    assert_eq!(
        send(
            &f,
            "DELETE",
            &path,
            &body,
            Some(&token),
            Some("tls-delete-role-0001")
        )
        .status,
        200
    );
    assert_eq!(
        send(
            &f,
            "DELETE",
            &path,
            &body,
            Some(&token),
            Some("tls-delete-role-0001")
        )
        .status,
        200
    );
    management_reply_error(
        &send(
            &f,
            "DELETE",
            "roles/owner",
            &serde_json::json!({"expected_version":"0".repeat(64)}),
            Some(&token),
            Some("tls-delete-owner-0001"),
        ),
        403,
        "management_forbidden",
    );
}
#[test]
fn permission_tls_automatic_access_and_one_time_invitations() {
    let f = Fixture::new("minecraft", 8, 16384, 5);
    let (_, _, token) = actor(&f, BuiltinRole::Owner);
    let access = send(
        &f,
        "GET",
        "access-sources",
        &serde_json::Value::Null,
        Some(&token),
        None,
    )
    .json()["data"]
        .clone();
    management_reply_error(
        &send(
            &f,
            "PATCH",
            "access-sources",
            &serde_json::json!({"expected_version":access["version"],"anonymous":"owner"}),
            Some(&token),
            Some("tls-access-unsafe-0001"),
        ),
        400,
        "invalid_request",
    );
    assert_eq!(
        send(
            &f,
            "PATCH",
            "access-sources",
            &serde_json::json!({"expected_version":access["version"],"whitelist_enabled":false}),
            Some(&token),
            Some("tls-access-safe-0001")
        )
        .status,
        200
    );
    management_reply_error(
        &send(
            &f,
            "POST",
            "invitations",
            &serde_json::json!({"role":"owner","expires_minutes":10}),
            Some(&token),
            Some("tls-invite-owner-0001"),
        ),
        403,
        "management_forbidden",
    );
    let body = serde_json::json!({"role":"viewer","expires_minutes":10});
    let first = send(
        &f,
        "POST",
        "invitations",
        &body,
        Some(&token),
        Some("tls-invite-once-0001"),
    );
    assert_eq!(first.status, 200);
    assert!(first.json()["data"]["code"].is_string());
    let retry = send(
        &f,
        "POST",
        "invitations",
        &body,
        Some(&token),
        Some("tls-invite-once-0001"),
    );
    assert_eq!(retry.status, 200);
    assert!(retry.json()["data"].get("code").is_none());
}
#[test]
fn permission_tls_device_scope_session_kill_and_success_audit() {
    let f = Fixture::new("minecraft", 8, 16384, 5);
    let (_, higher, _) = actor(&f, BuiltinRole::Owner);
    let (_, self_device, token) = actor(&f, BuiltinRole::Administrator);
    let (_, equal, _) = actor(&f, BuiltinRole::Administrator);
    let (lower, device, ls) = actor(&f, BuiltinRole::Viewer);
    for d in [higher, equal] {
        management_reply_error(
            &send(
                &f,
                "DELETE",
                &format!("devices/{d}"),
                &serde_json::Value::Null,
                Some(&token),
                Some("tls-device-denied-0001"),
            ),
            403,
            "management_forbidden",
        );
    }
    assert_eq!(
        send(
            &f,
            "DELETE",
            &format!("devices/{device}"),
            &serde_json::Value::Null,
            Some(&token),
            Some("tls-device-revoke-0001")
        )
        .status,
        200
    );
    management_reply_error(
        &send(
            &f,
            "GET",
            &format!("users/{lower}"),
            &serde_json::Value::Null,
            Some(&ls),
            None,
        ),
        401,
        "management_session_invalid",
    );
    let audit = send(
        &f,
        "GET",
        "audit?limit=100",
        &serde_json::Value::Null,
        Some(&token),
        None,
    );
    assert_eq!(audit.status, 200);
    assert!(
        String::from_utf8(audit.body)
            .unwrap()
            .contains("management.permissions.mutate")
    );
    assert_eq!(
        send(
            &f,
            "DELETE",
            &format!("devices/{self_device}"),
            &serde_json::Value::Null,
            Some(&token),
            Some("tls-device-self-0001")
        )
        .status,
        200
    );
    management_reply_error(
        &send(
            &f,
            "GET",
            "roles",
            &serde_json::Value::Null,
            Some(&token),
            None,
        ),
        401,
        "management_session_invalid",
    );
}
#[test]
fn permission_tls_no_pack_authority_unknown_fields_and_required_idempotency() {
    let f = Fixture::new("minecraft", 8, 16384, 5);
    let (_, _, token) = actor(&f, BuiltinRole::Owner);
    let pack = f.device_session();
    for path in ["users", "roles", "access-sources"] {
        management_reply_error(
            &send(&f, "GET", path, &serde_json::Value::Null, None, None),
            401,
            "management_session_invalid",
        );
        management_reply_error(
            &send(&f, "GET", path, &serde_json::Value::Null, Some(&pack), None),
            403,
            "management_forbidden",
        );
    }
    let input = serde_json::json!({"role":"viewer","expires_minutes":10});
    management_reply_error(
        &send(&f, "POST", "invitations", &input, Some(&token), None),
        400,
        "invalid_request",
    );
    management_reply_error(
        &send(
            &f,
            "POST",
            "invitations",
            &serde_json::json!({"role":"viewer","expires_minutes":10,"extra":true}),
            Some(&token),
            Some("tls-unknown-field-0001"),
        ),
        400,
        "invalid_request",
    );
}
