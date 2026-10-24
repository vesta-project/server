use super::*;
use vesta_control::{
    identity::BuiltinRole,
    motd::{Document, State},
};
#[test]
fn presentation_public_state_and_authorized_policy_preview_share_safe_templates() {
    let f = Fixture::new("minecraft", 8, 65_536, 5);
    let public = f.request("GET", "/api/v3/status", b"", None);
    assert_eq!(public.status, 200);
    management_reply_error(
        &f.request("GET", "/api/v3/status?private=1", b"", None),
        400,
        "invalid_request",
    );
    assert_eq!(public.json()["state"], "offline");
    assert_eq!(public.json()["motd"]["state"], "offline");
    for private in [
        "principal_id",
        "device_id",
        "generation",
        "working_directory",
    ] {
        assert!(!String::from_utf8_lossy(&public.body).contains(private));
    }
    let (device, key) = enroll_management(&f, BuiltinRole::Owner);
    let token = management_session(&f, &device, &key);
    let policy_path = "/api/v3/manage/motd/policy";
    management_reply_error(
        &f.request("GET", policy_path, b"", None),
        401,
        "management_session_invalid",
    );
    let before = f.request("GET", policy_path, b"", Some(&token));
    assert_eq!(before.status, 200);
    assert!(before.json()["data"]["revision"].is_null());
    let mut document = Document::default();
    document.states.get_mut(&State::Offline).unwrap().template =
        "§a{server_name}\n{state} {online_players}/{max_players}".into();
    let put = serde_json::json!({"expected_revision":null,"document":document});
    let bytes = serde_json::to_vec(&put).unwrap();
    let saved = f.request("PUT", policy_path, &bytes, Some(&token));
    assert_eq!(saved.status, 200);
    assert_eq!(saved.json()["data"]["revision"], 1);
    assert_eq!(
        f.request("PUT", policy_path, &bytes, Some(&token)).status,
        409
    );
    let preview = f.request(
        "POST",
        "/api/v3/manage/motd/preview",
        &serde_json::to_vec(&serde_json::json!({"state":"offline","document":document})).unwrap(),
        Some(&token),
    );
    assert_eq!(preview.status, 200);
    assert_eq!(
        preview.json()["data"]["preview"]["plain_lines"][1],
        "offline 0/0"
    );
    let public = f.request("GET", "/api/v3/status", b"", None);
    assert_eq!(public.status, 200);
    assert_eq!(public.json()["motd"]["plain_lines"][1], "offline 0/0");
    let mut invalid = document;
    invalid.states.get_mut(&State::Offline).unwrap().template = "{password}".into();
    management_reply_error(
        &f.request(
            "POST",
            "/api/v3/manage/motd/preview",
            &serde_json::to_vec(&serde_json::json!({"state":"offline","document":invalid}))
                .unwrap(),
            Some(&token),
        ),
        400,
        "invalid_request",
    );
}
#[test]
fn sleep_policy_defaults_off_uses_optimistic_revision_and_needs_configuration_permission() {
    let f = Fixture::new("minecraft", 8, 4096, 5);
    let (device, key) = enroll_management(&f, BuiltinRole::Owner);
    let token = management_session(&f, &device, &key);
    let path = "/api/v3/manage/sleep/policy";
    let initial = f.request("GET", path, b"", Some(&token));
    assert_eq!(initial.status, 200);
    assert_eq!(initial.json()["data"]["revision"], 0);
    assert_eq!(initial.json()["data"]["policy"]["enabled"], false);
    assert_eq!(
        initial.json()["data"]["sleep_blocker"],
        serde_json::json!({"reason":"disabled"})
    );
    let document =
        serde_json::json!({"expected_revision":0,"policy":vesta_control::sleep::Policy::default()});
    let bytes = serde_json::to_vec(&document).unwrap();
    assert_eq!(f.request("PUT", path, &bytes, Some(&token)).status, 200);
    management_reply_error(
        &f.request("PUT", path, &bytes, Some(&token)),
        409,
        "sleep_policy_conflict",
    );
    let (device, key) = enroll_management(&f, BuiltinRole::Viewer);
    let viewer = management_session(&f, &device, &key);
    management_reply_error(
        &f.request("PUT", path, &bytes, Some(&viewer)),
        403,
        "management_forbidden",
    );
    let mut invalid = document;
    invalid["policy"]["idle_seconds"] = serde_json::json!(1);
    invalid["expected_revision"] = serde_json::json!(1);
    management_reply_error(
        &f.request(
            "PUT",
            path,
            &serde_json::to_vec(&invalid).unwrap(),
            Some(&token),
        ),
        400,
        "invalid_request",
    );
}
