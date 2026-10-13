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
    let point = &range.json()["data"]["points"][0];
    assert!(point["numeric"]["avg"].is_null());
    assert_eq!(point["numeric"]["unknown_count"], 1);
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
