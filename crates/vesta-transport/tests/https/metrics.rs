use super::*;
#[test]
fn management_metrics_routes_are_bounded_passive_and_identity_protected() {
    use vesta_control::identity::BuiltinRole;
    let f = Fixture::new("minecraft", 8, 4096, 5);
    let (viewer_device, viewer_key) = enroll_management(&f, BuiltinRole::Viewer);
    let viewer = management_session(&f, &viewer_device, &viewer_key);
    let (moderator_device, moderator_key) = enroll_management(&f, BuiltinRole::Moderator);
    let moderator = management_session(&f, &moderator_device, &moderator_key);
    for path in [
        "/api/v3/manage/metrics/current",
        "/api/v3/manage/metrics?series=tps&from=0&to=1",
        "/api/v3/manage/players/activity",
    ] {
        management_reply_error(
            &f.request("GET", path, b"", None),
            401,
            "management_session_invalid",
        );
    }
    management_reply_error(
        &f.request(
            "GET",
            "/api/v3/manage/metrics/current",
            b"",
            Some(&moderator),
        ),
        403,
        "management_forbidden",
    );
    let until = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let current = loop {
        let current = f.request("GET", "/api/v3/manage/metrics/current", b"", Some(&viewer));
        assert_eq!(current.status, 200);
        if current.json()["data"]["sample"].is_object() {
            break current.json();
        }
        assert!(std::time::Instant::now() < until);
        std::thread::sleep(std::time::Duration::from_millis(20));
    };
    let sample = &current["data"]["sample"];
    assert!(sample["values"]["agent_memory_bytes"].as_f64().unwrap() > 0.0);
    for name in [
        "game_memory_bytes",
        "players_online",
        "players_max",
        "tps",
        "mspt",
        "game_network_received_bytes",
    ] {
        assert!(sample["values"][name].is_null());
        assert!(
            sample["unknown"]
                .as_array()
                .unwrap()
                .contains(&serde_json::json!(name))
        );
    }
    let at = sample["sampled_at"].as_i64().unwrap();
    let range = f.request(
        "GET",
        &format!("/api/v3/manage/metrics?series=tps&from={at}&to={at}&resolution=raw"),
        b"",
        Some(&viewer),
    );
    assert_eq!(range.status, 200);
    let data = &range.json()["data"];
    assert_eq!(data["timestamps"], serde_json::json!([at]));
    assert!(data["series"]["tps"]["avg"][0].is_null());
    assert_eq!(data["series"]["tps"]["known_count"][0], 0);
    assert_eq!(data["series"]["tps"]["unknown_count"][0], 1);
    for path in [
        "metrics/current?extra=1",
        "metrics?series=bogus&from=0&to=1",
        "metrics?series=tps&from=2&to=1",
        "metrics?series=tps&from=0&to=1&from=0",
        "players/activity?limit=101",
        "players/activity?after=-1",
    ] {
        management_reply_error(
            &f.request("GET", &format!("/api/v3/manage/{path}"), b"", Some(&viewer)),
            400,
            "invalid_request",
        );
    }
    f.host().database().write(|c| {
        for (id,name,kind) in [(1_000_001,"PrivateAlex","join"),(1_000_002,"PrivateAlex","leave")] {
            c.execute("INSERT INTO metrics_activity(id,at,generation,name,kind,bytes) VALUES(?,?,'00112233445566778899aabbccddeeff',?,?,100)",rusqlite::params![id,at,name,kind])?;
        }
        c.execute("UPDATE metrics_status SET storage_bytes=storage_bytes+200 WHERE singleton=1",[])?;Ok(())
    }).unwrap();
    let first = f.request(
        "GET",
        "/api/v3/manage/players/activity?after=1000000&limit=1",
        b"",
        Some(&moderator),
    );
    assert_eq!(first.status, 200);
    assert_eq!(first.json()["data"]["items"][0]["kind"], "join");
    assert_eq!(first.json()["data"]["next_after"], 1_000_001);
    let second = f.request(
        "GET",
        "/api/v3/manage/players/activity?after=1000001&limit=1",
        b"",
        Some(&moderator),
    );
    assert_eq!(second.json()["data"]["items"][0]["kind"], "leave");
    assert!(second.json()["data"]["next_after"].is_null());
    f.request("GET", "/api/v3/manage/metrics/current", b"", Some(&viewer))
        .hides("PrivateAlex");
    f.host().database().write(|c| {
        let principal:String=c.query_row("SELECT principal_id FROM devices WHERE id=?",[&viewer_device],|r|r.get(0))?;
        c.execute("INSERT INTO principal_overrides(id,principal_id,capability,scope,effect,granted_by,created_at) VALUES(?,?,'players.view','*','deny','fixture',?)",rusqlite::params!["99999999-aaaa-4bbb-8ccc-dddddddddddd",principal,at])?;Ok(())
    }).unwrap();
    management_reply_error(
        &f.request("GET", "/api/v3/manage/players/activity", b"", Some(&viewer)),
        403,
        "management_forbidden",
    );
    assert_eq!(
        f.request("GET", "/api/v3/manage/metrics/current", b"", Some(&viewer))
            .status,
        200
    );
}

fn metrics_raw(
    f: &Fixture,
    path: &str,
    session: Option<&str>,
    validator: Option<&str>,
) -> WireReply {
    let authorization = session
        .map(|token| format!("Authorization: Bearer {token}\r\n"))
        .unwrap_or_default();
    let validator = validator
        .map(|tag| format!("If-None-Match: {tag}\r\n"))
        .unwrap_or_default();
    f.raw(
        format!(
            "GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n{authorization}{validator}\r\n"
        )
        .as_bytes(),
    )
}

#[test]
fn metrics_policy_uses_host_fallback_and_revisioned_storage_with_config_capabilities() {
    use vesta_control::identity::BuiltinRole;

    let f = Fixture::new("minecraft", 8, 4096, 5);
    let (viewer_device, viewer_key) = enroll_management(&f, BuiltinRole::Viewer);
    let viewer = management_session(&f, &viewer_device, &viewer_key);
    let (owner_device, owner_key) = enroll_management(&f, BuiltinRole::Owner);
    let owner = management_session(&f, &owner_device, &owner_key);

    f.host()
        .database()
        .write(|c| {
            let principal: String = c.query_row(
                "SELECT principal_id FROM devices WHERE id=?",
                [&viewer_device],
                |r| r.get(0),
            )?;
            c.execute(
                "INSERT INTO principal_overrides(id,principal_id,capability,scope,effect,granted_by,created_at) VALUES('metrics-config-view-only',?,'config.view','*','allow','fixture',?)",
                rusqlite::params![principal, f.host().database().clock().millis()],
            )?;
            Ok(())
        })
        .unwrap();

    let fallback = f.request("GET", "/api/v3/manage/metrics/policy", b"", Some(&viewer));
    assert_eq!(fallback.status, 200);
    let fallback = fallback.json()["data"].clone();
    assert_eq!(fallback["source"], "host_config");
    assert!(fallback["revision"].is_null());
    assert_eq!(fallback["settings"]["sample_seconds"], 10);
    assert_eq!(fallback["command_polling"]["enabled"], false);
    assert_eq!(fallback["command_polling"]["interval_seconds"], 60);
    assert_eq!(fallback["command_polling_writes_console"], true);

    let settings = serde_json::to_value(vesta_control::config::MetricsSettings::default()).unwrap();
    let document = serde_json::json!({
        "expected_revision": null,
        "settings": settings,
        "command_polling": {"enabled": true, "interval_seconds": 120}
    });
    let body = serde_json::to_vec(&document).unwrap();
    management_reply_error(
        &f.request("PUT", "/api/v3/manage/metrics/policy", &body, Some(&viewer)),
        403,
        "management_forbidden",
    );

    let stored = f.request("PUT", "/api/v3/manage/metrics/policy", &body, Some(&owner));
    assert_eq!(stored.status, 200);
    assert_eq!(stored.json()["data"]["source"], "stored");
    assert_eq!(stored.json()["data"]["revision"], 1);
    let readback = f.request("GET", "/api/v3/manage/metrics/policy", b"", Some(&owner));
    assert_eq!(readback.status, 200);
    let readback = readback.json()["data"].clone();
    assert_eq!(readback["source"], "stored");
    assert_eq!(readback["revision"], 1);
    assert_eq!(readback["settings"]["sample_seconds"], 10);
    assert_eq!(readback["command_polling"]["enabled"], true);
    assert_eq!(readback["command_polling"]["interval_seconds"], 120);

    let mut stale = document.clone();
    stale["expected_revision"] = serde_json::Value::Null;
    management_reply_error(
        &f.request(
            "PUT",
            "/api/v3/manage/metrics/policy",
            &serde_json::to_vec(&stale).unwrap(),
            Some(&owner),
        ),
        409,
        "metrics_policy_conflict",
    );

    let mut invalid = document.clone();
    invalid["expected_revision"] = serde_json::json!(1);
    invalid["settings"]["sample_seconds"] = serde_json::json!(4);
    management_reply_error(
        &f.request(
            "PUT",
            "/api/v3/manage/metrics/policy",
            &serde_json::to_vec(&invalid).unwrap(),
            Some(&owner),
        ),
        400,
        "invalid_request",
    );
    let mut unexpected = document;
    unexpected["unexpected"] = serde_json::json!(true);
    management_reply_error(
        &f.request(
            "PUT",
            "/api/v3/manage/metrics/policy",
            &serde_json::to_vec(&unexpected).unwrap(),
            Some(&owner),
        ),
        400,
        "invalid_request",
    );

    let audit_count: i64 = f
        .host()
        .database()
        .read(|c| {
            c.query_row(
                "SELECT COUNT(*) FROM audit_events WHERE action='metrics.policy.update' AND target='metrics-policy'",
                [],
                |r| r.get(0),
            )
            .map_err(Into::into)
        })
        .unwrap();
    assert_eq!(audit_count, 1);
}

#[test]
fn metrics_etags_are_exact_authorized_and_cursor_only_ranges_are_empty() {
    use vesta_control::identity::BuiltinRole;

    let f = Fixture::new("minecraft", 8, 4096, 5);
    let (device, key) = enroll_management(&f, BuiltinRole::Owner);
    let session = management_session(&f, &device, &key);

    let settings = vesta_control::config::MetricsSettings {
        enabled: false,
        ..vesta_control::config::MetricsSettings::default()
    };
    let disabled = serde_json::json!({"expected_revision": null, "settings": settings});
    assert_eq!(
        f.request(
            "PUT",
            "/api/v3/manage/metrics/policy",
            &serde_json::to_vec(&disabled).unwrap(),
            Some(&session)
        )
        .status,
        200
    );
    let current = metrics_raw(&f, "/api/v3/manage/metrics/current", Some(&session), None);
    assert_eq!(current.status, 200);
    let mut current_tag = current.headers.get("etag").expect("current ETag").clone();
    let mut not_modified = metrics_raw(
        &f,
        "/api/v3/manage/metrics/current",
        Some(&session),
        Some(&current_tag),
    );
    // One sampler observation may already be in flight when disable commits.
    // A changed revision correctly yields 200; retry using its fresh validator.
    for _ in 0..8 {
        if not_modified.status == 304 {
            break;
        }
        assert_eq!(not_modified.status, 200);
        current_tag = not_modified
            .headers
            .get("etag")
            .expect("fresh current ETag")
            .clone();
        not_modified = metrics_raw(
            &f,
            "/api/v3/manage/metrics/current",
            Some(&session),
            Some(&current_tag),
        );
    }
    assert_eq!(not_modified.status, 304);
    assert!(not_modified.body.is_empty());
    assert_eq!(not_modified.headers.get("etag"), Some(&current_tag));
    management_reply_error(
        &metrics_raw(
            &f,
            "/api/v3/manage/metrics/current",
            None,
            Some(&current_tag),
        ),
        401,
        "management_session_invalid",
    );
    for malformed in ["*", r#"W/"weak""#, r#""one",two"#] {
        management_reply_error(
            &metrics_raw(
                &f,
                "/api/v3/manage/metrics/current",
                Some(&session),
                Some(malformed),
            ),
            400,
            "invalid_request",
        );
    }

    let path = "/api/v3/manage/metrics?series=tps&from=0&to=1&resolution=raw";
    let range = metrics_raw(&f, path, Some(&session), None);
    assert_eq!(range.status, 200);
    let range_tag = range.headers.get("etag").expect("range ETag").clone();
    let unchanged = metrics_raw(&f, path, Some(&session), Some(&range_tag));
    assert_eq!(unchanged.status, 304);
    assert!(unchanged.body.is_empty());
    assert_eq!(unchanged.headers.get("etag"), Some(&range_tag));
    let stale = metrics_raw(&f, path, Some(&session), Some(r#""metrics-0-stale""#));
    assert_eq!(stale.status, 200);
    assert!(stale.json()["data"].is_object());

    let cursor = range.json()["data"]["cursor"].as_i64().unwrap();
    let empty = metrics_raw(&f, &format!("{path}&after={cursor}"), Some(&session), None);
    assert_eq!(empty.status, 200);
    let data = empty.json()["data"].clone();
    assert_eq!(data["timestamps"], serde_json::json!([]));
    assert_eq!(data["series"]["tps"]["avg"], serde_json::json!([]));
    assert_eq!(
        data["series"]["tps"]["unknown_count"],
        serde_json::json!([])
    );
    let changed_policy = serde_json::json!({
        "expected_revision": 1,
        "settings": vesta_control::config::MetricsSettings {
            enabled: false, sample_seconds: 5,
            ..vesta_control::config::MetricsSettings::default()
        }
    });
    assert_eq!(
        f.request(
            "PUT",
            "/api/v3/manage/metrics/policy",
            &serde_json::to_vec(&changed_policy).unwrap(),
            Some(&session)
        )
        .status,
        200
    );
    let changed = metrics_raw(&f, path, Some(&session), Some(&range_tag));
    assert_eq!(
        changed.status, 200,
        "policy changes invalidate range validators before sampling"
    );
    assert_ne!(changed.headers.get("etag"), Some(&range_tag));
    assert_eq!(changed.json()["data"]["bucket_millis"], 5000);
    let incremental = metrics_raw(&f, &format!("{path}&after={cursor}"), Some(&session), None);
    assert_eq!(incremental.json()["data"]["retention_gap"], true);
}

#[test]
fn status_etag_is_strong_conditional_and_never_bypasses_authority() {
    let f = Fixture::inert();
    let (device, key) = enroll_management(&f, vesta_control::identity::BuiltinRole::Owner);
    let token = management_session(&f, &device, &key);
    let path = "/api/v3/manage/server/status";
    let first = metrics_raw(&f, path, Some(&token), None);
    assert_eq!(first.status, 200);
    let tag = &first.headers["etag"];
    assert!(tag.starts_with("\"status-") && tag.ends_with('"'));
    let repeat = metrics_raw(&f, path, Some(&token), None);
    assert_eq!(
        repeat.body, first.body,
        "strong validator must identify byte-identical representation"
    );
    let unchanged = metrics_raw(&f, path, Some(&token), Some(tag));
    assert_eq!(unchanged.status, 304);
    assert!(unchanged.body.is_empty());
    assert_eq!(unchanged.headers.get("etag"), Some(tag));
    assert!(!unchanged.headers.contains_key("content-length"));
    management_reply_error(
        &metrics_raw(&f, path, None, Some(tag)),
        401,
        "management_session_invalid",
    );
    for malformed in ["*", "W/\"weak\"", "\"one\",\"two\""] {
        management_reply_error(
            &metrics_raw(&f, path, Some(&token), Some(malformed)),
            400,
            "invalid_request",
        );
    }
    let duplicate = f.raw(format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nAuthorization: Bearer {token}\r\nIf-None-Match: {tag}\r\nIf-None-Match: {tag}\r\n\r\n").as_bytes());
    management_reply_error(&duplicate, 400, "invalid_request");
    f.host().database().write(|c| {
        let principal:String=c.query_row("SELECT principal_id FROM devices WHERE id=?",[&device],|r|r.get(0))?;
        c.execute("INSERT INTO principal_overrides(id,principal_id,capability,scope,effect,granted_by,created_at) VALUES('etag-status-deny',?,'status.view','*','deny','fixture',0)",[principal])?;
        Ok(())
    }).unwrap();
    management_reply_error(
        &metrics_raw(&f, path, Some(&token), Some(tag)),
        403,
        "management_forbidden",
    );
    f.host()
        .database()
        .write(|c| {
            c.execute(
                "DELETE FROM principal_overrides WHERE id='etag-status-deny'",
                [],
            )?;
            Ok(())
        })
        .unwrap();
    let generation = management_start_inert(&f, &token);
    let changed = metrics_raw(&f, path, Some(&token), Some(tag));
    assert_eq!(changed.status, 200);
    assert_ne!(&changed.headers["etag"], tag);
    assert_eq!(changed.json()["data"]["generation"], generation);
    f.host().management().revoke_local(&device).unwrap();
    management_reply_error(
        &metrics_raw(&f, path, Some(&token), Some(&changed.headers["etag"])),
        401,
        "management_session_invalid",
    );
}
