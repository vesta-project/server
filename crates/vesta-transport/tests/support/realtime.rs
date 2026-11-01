use super::*;
use tokio_tungstenite::tungstenite::{
    self, Message, client::IntoClientRequest, protocol::frame::coding::CloseCode,
};
type RealtimePeer = tungstenite::WebSocket<TlsStream>;
impl Fixture {
    fn ticket(&self, session: &str, realtime: bool) -> String {
        let path = if realtime {
            "/api/v2/realtime/ticket"
        } else {
            "/api/v2/pack/ticket"
        };
        let reply = self.request("POST", path, b"", Some(session));
        assert_eq!(reply.status, 200);
        reply.json()["ticket"].as_str().unwrap().to_owned()
    }
    fn websocket(&self, ticket: &str, installed: u64) -> Result<RealtimePeer, u16> {
        let mut request = format!(
            "wss://localhost:{}/api/v2/realtime?installed={installed}",
            self.address.port()
        )
        .into_client_request()
        .unwrap();
        request
            .headers_mut()
            .insert("authorization", format!("Bearer {ticket}").parse().unwrap());
        match tungstenite::client(request, self.stream()) {
            Ok((peer, reply)) => {
                assert_eq!(reply.status().as_u16(), 101);
                assert!(
                    !reply
                        .headers()
                        .keys()
                        .any(|name| name.as_str().starts_with("access-control-"))
                );
                Ok(peer)
            }
            Err(tungstenite::HandshakeError::Failure(tungstenite::Error::Http(reply))) => {
                Err(reply.status().as_u16())
            }
            Err(error) => panic!("fixture upgrade failed: {error}"),
        }
    }
    fn direct_ticket(&self, session: &str) -> String {
        self.host()
            .pack_sessions()
            .issue_ticket_for(
                session,
                vesta_control::pack_access::TicketPurpose::Realtime,
                Instant::now(),
                &self.host().pack_policy().unwrap(),
            )
            .unwrap()
            .expose()
            .to_owned()
    }
}
fn realtime_json(peer: &mut RealtimePeer) -> serde_json::Value {
    // Native clients service WebSocket control frames while waiting for a
    // notice. Creating many real signed sessions may cross a heartbeat tick.
    for _ in 0..8 {
        match peer.read().unwrap() {
            Message::Text(text) => return serde_json::from_str(&text).unwrap(),
            Message::Ping(_) => peer.flush().unwrap(), // flush Tungstenite's automatic pong
            other => panic!("expected JSON notice, got {other:?}"),
        }
    }
    panic!("too many control frames before a JSON notice");
}
fn realtime_close(peer: &mut RealtimePeer, code: CloseCode) {
    for _ in 0..8 {
        match peer.read().unwrap() {
            Message::Close(Some(frame)) => {
                assert_eq!(frame.code, code);
                return;
            }
            Message::Ping(_) => peer.flush().unwrap(),
            Message::Text(text) => assert_eq!(
                serde_json::from_str::<serde_json::Value>(&text).unwrap()["type"],
                "heartbeat"
            ),
            other => panic!("expected close, got {other:?}"),
        }
    }
    panic!("too many queued notices before close");
}

#[test]
fn realtime_browser_origins_are_refused_before_consuming_native_ticket() {
    let f = Fixture::new("minecraft-or-password", 8, 4096, 10);
    let owner = f.owner();
    let session = f.password_session(&owner);
    let ticket = f.ticket(&session, true);
    for origin in [
        "https://untrusted.invalid",
        "null",
        "https://localhost",
        "",
        "https://first.invalid\r\nOrigin: https://second.invalid",
    ] {
        let request = format!(
            "GET /api/v2/realtime?installed=0 HTTP/1.1\r\nHost: localhost\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nAuthorization: Bearer {ticket}\r\nOrigin: {origin}\r\n\r\n"
        );
        let reply = f.raw(request.as_bytes());
        reply.error(403, "browser_origin_forbidden");
        reply.hides(&ticket);
        assert!(
            !reply
                .headers
                .keys()
                .any(|name| name.starts_with("access-control-"))
        );
        assert_eq!(f.host().pack_sessions().len().unwrap(), (1, 1));
    }
    let mut native = f.websocket(&ticket, 0).unwrap();
    assert_eq!(realtime_json(&mut native)["type"], "hello");
    assert_eq!(f.websocket(&ticket, 0).err(), Some(401));
}

#[test]
fn realtime_ticket_single_use_purpose_binding_and_download_ticket_redemption() {
    let f = Fixture::new("minecraft-or-password", 16, 4096, 10);
    let owner = f.owner();
    let session = f.password_session(&owner);
    f.publish(&owner, UpdatePolicy::Optional, b"first", "1");
    let download = f.ticket(&session, false);
    assert_eq!(f.websocket(&download, 0).err(), Some(401));
    f.request("GET", "/api/v2/pack/artifact", b"", Some(&download))
        .error(401, "pack_session_invalid");
    let realtime = f.ticket(&session, true);
    f.request("GET", "/api/v2/pack/artifact", b"", Some(&realtime))
        .error(401, "pack_session_invalid");
    assert_eq!(f.websocket(&realtime, 0).err(), Some(401));
    let download = f.ticket(&session, false);
    assert_eq!(
        f.request("GET", "/api/v2/pack/artifact", b"", Some(&download))
            .body,
        b"first"
    );
    f.request("GET", "/api/v2/pack/artifact", b"", Some(&download))
        .error(401, "pack_session_invalid");
    let realtime = f.ticket(&session, true);
    let mut peer = f.websocket(&realtime, 0).unwrap();
    assert_eq!(realtime_json(&mut peer)["type"], "hello");
    assert_eq!(f.websocket(&realtime, 0).err(), Some(401));
    assert_eq!(f.websocket(&session, 0).err(), Some(401));
}

#[test]
fn realtime_hello_update_and_required_minimum_are_delivered_with_client_status() {
    let f = Fixture::new("minecraft-or-password", 16, 4096, 10);
    let owner = f.owner();
    let session = f.password_session(&owner);
    f.publish(&owner, UpdatePolicy::Optional, b"first", "1");
    let mut peer = f.websocket(&f.ticket(&session, true), 1).unwrap();
    assert_eq!(
        realtime_json(&mut peer),
        serde_json::json!({"type":"hello", "revision":1, "minimum_revision":1, "status":"current"})
    );
    f.publish(&owner, UpdatePolicy::Optional, b"second", "2");
    assert_eq!(
        realtime_json(&mut peer),
        serde_json::json!({"type":"pack_update", "revision":2, "minimum_revision":1, "status":"update_available"})
    );
    f.publish(&owner, UpdatePolicy::Required, b"third", "3");
    assert_eq!(
        realtime_json(&mut peer),
        serde_json::json!({"type":"pack_update", "revision":3, "minimum_revision":3, "status":"update_required"})
    );
    // Independent stores for the same canonical database share the watch signal.
    let releases = vesta_control::packs::PackReleases::open(f.host().database()).unwrap();
    std::fs::write(f.temp.path().join("game/config/pack.zip"), b"fourth").unwrap();
    let archive = f.host().pack_preparations()[0]
        .archive(&vesta_control::files::RelativePath::parse("pack.zip").unwrap())
        .unwrap();
    let plan = releases.plan(&owner, 3, UpdatePolicy::Optional).unwrap();
    releases
        .publish_archive(&owner, &plan, &archive, "4", false)
        .unwrap();
    assert_eq!(realtime_json(&mut peer)["revision"], 4);
}

#[test]
fn realtime_session_expiry_closes_socket_without_waiting_for_heartbeat() {
    let f = Fixture::new("minecraft-or-password", 8, 4096, 10);
    let owner = f.owner();
    let session = f.password_session(&owner);
    let mut peer = f.websocket(&f.direct_ticket(&session), 0).unwrap();
    assert_eq!(
        realtime_json(&mut peer),
        serde_json::json!({"type":"hello", "revision":0, "minimum_revision":0, "status":null})
    );
    let start = Instant::now();
    f.clock
        .0
        .fetch_add(900_001, std::sync::atomic::Ordering::SeqCst);
    f.publish(&owner, UpdatePolicy::Optional, b"expiry wake", "1");
    realtime_close(&mut peer, CloseCode::Policy);
    assert!(start.elapsed() < Duration::from_secs(3));
}

#[test]
fn realtime_session_revocation_and_policy_rotation_close_before_push() {
    let f = Fixture::new("minecraft-or-password", 8, 4096, 10);
    let owner = f.owner();
    let session = f.password_session(&owner);
    let mut peer = f.websocket(&f.ticket(&session, true), 0).unwrap();
    realtime_json(&mut peer);
    f.host().pack_sessions().revoke(&session).unwrap();
    f.publish(&owner, UpdatePolicy::Optional, b"first", "1");
    realtime_close(&mut peer, CloseCode::Policy);
    let session = f.granted_session();
    let mut peer = f.websocket(&f.direct_ticket(&session), 0).unwrap();
    realtime_json(&mut peer);
    f.host()
        .pack_passwords()
        .unwrap()
        .revoke(&owner, PasswordRevision::new(1).unwrap())
        .unwrap();
    f.publish(&owner, UpdatePolicy::Optional, b"second", "2");
    realtime_close(&mut peer, CloseCode::Policy);
}

#[test]
fn realtime_unsolicited_text_and_binary_frames_close_with_policy_violation() {
    let f = Fixture::new("minecraft-or-password", 8, 4096, 10);
    let owner = f.owner();
    let session = f.password_session(&owner);
    for message in [
        Message::Text("{\"command\":\"console\"}".to_owned()),
        Message::Binary(vec![1]),
    ] {
        let mut peer = f.websocket(&f.ticket(&session, true), 0).unwrap();
        realtime_json(&mut peer);
        peer.send(message).unwrap();
        realtime_close(&mut peer, CloseCode::Policy);
    }
}

#[test]
fn realtime_oversize_frame_closes_with_policy_violation() {
    let f = Fixture::new("minecraft-or-password", 8, 4096, 10);
    let owner = f.owner();
    let session = f.password_session(&owner);
    let mut peer = f.websocket(&f.ticket(&session, true), 0).unwrap();
    realtime_json(&mut peer);
    peer.send(Message::Binary(vec![0; 1025])).unwrap();
    realtime_close(&mut peer, CloseCode::Policy);
}

#[test]
fn realtime_per_session_connection_cap_releases_on_close() {
    let f = Fixture::new("minecraft-or-password", 16, 4096, 10);
    let owner = f.owner();
    let session = f.password_session(&owner);
    let mut first = f.websocket(&f.ticket(&session, true), 0).unwrap();
    realtime_json(&mut first);
    let mut second = f.websocket(&f.ticket(&session, true), 0).unwrap();
    realtime_json(&mut second);
    let denied = f.ticket(&session, true);
    assert_eq!(f.websocket(&denied, 0).err(), Some(503));
    assert_eq!(f.websocket(&denied, 0).err(), Some(401));
    first.close(None).unwrap();
    assert!(matches!(first.read().unwrap(), Message::Close(_)));
    let mut replacement = f.websocket(&f.ticket(&session, true), 0).unwrap();
    assert_eq!(realtime_json(&mut replacement)["type"], "hello");
}

#[test]
fn realtime_global_connection_cap_and_shutdown_join_all_sockets() {
    let mut f = Fixture::new("minecraft-or-password", 64, 4096, 10);
    let owner = f.owner();
    f.password_session(&owner);
    // Complete real key enrollment/signatures before leaving any socket idle.
    // The capacity test is not a test of clients ignoring heartbeat controls
    // while 33 durable authentication transactions are being performed.
    let sessions: Vec<_> = (0..33).map(|_| f.granted_session()).collect();
    let mut peers = Vec::new();
    for session in &sessions[..32] {
        let mut peer = f.websocket(&f.direct_ticket(session), 0).unwrap();
        realtime_json(&mut peer);
        peers.push(peer);
    }
    let session = &sessions[32];
    assert_eq!(f.websocket(&f.direct_ticket(session), 0).err(), Some(503));
    f.publish(
        &owner,
        UpdatePolicy::Optional,
        b"full-budget broadcast",
        "1",
    );
    for peer in &mut peers {
        let mut received = realtime_json(peer);
        for _ in 0..4 {
            if received["type"] != "heartbeat" {
                break;
            }
            received = realtime_json(peer);
        }
        assert_eq!(
            received,
            serde_json::json!({"type":"pack_update", "revision":1, "minimum_revision":1, "status":"update_required"})
        );
    }
    // Real clients read the bounded close while shutdown is happening, rather
    // than leaving all 32 sockets unread until after their server tasks join.
    std::thread::scope(|scope| {
        let readers: Vec<_> = peers
            .iter_mut()
            .map(|peer| scope.spawn(move || realtime_close(peer, CloseCode::Away)))
            .collect();
        f.finish();
        for reader in readers {
            reader.join().unwrap();
        }
    });
}

#[test]
fn realtime_control_ping_is_answered_and_socket_outlives_http_request_timeout() {
    let f = Fixture::new("minecraft-or-password", 8, 4096, 3);
    let owner = f.owner();
    let session = f.password_session(&owner);
    let mut peer = f.websocket(&f.ticket(&session, true), 0).unwrap();
    realtime_json(&mut peer);
    std::thread::sleep(Duration::from_millis(3100));
    peer.send(Message::Ping(b"owned-peer".to_vec())).unwrap();
    assert_eq!(peer.read().unwrap(), Message::Pong(b"owned-peer".to_vec()));
    f.publish(&owner, UpdatePolicy::Optional, b"first", "1");
    assert_eq!(realtime_json(&mut peer)["type"], "pack_update");
}

#[test]
fn realtime_heartbeat_rechecks_policy_even_without_publication() {
    let f = Fixture::new("minecraft-or-password", 8, 4096, 10);
    let owner = f.owner();
    let session = f.password_session(&owner);
    let mut peer = f.websocket(&f.ticket(&session, true), 0).unwrap();
    realtime_json(&mut peer);
    peer.get_mut()
        .sock
        .set_read_timeout(Some(Duration::from_secs(12)))
        .unwrap();
    assert_eq!(
        realtime_json(&mut peer),
        serde_json::json!({"type":"heartbeat"})
    );
    assert!(matches!(peer.read().unwrap(), Message::Ping(_)));
    peer.flush().unwrap();
    f.host().pack_sessions().revoke(&session).unwrap();
    realtime_close(&mut peer, CloseCode::Policy);
}

#[test]
fn realtime_heartbeat_delivers_releases_committed_outside_this_process_watch() {
    let f = Fixture::new("minecraft-or-password", 8, 4096, 10);
    let owner = f.owner();
    let session = f.password_session(&owner);
    let first = f.publish(&owner, UpdatePolicy::Optional, b"first", "1");
    let mut peer = f.websocket(&f.ticket(&session, true), 1).unwrap();
    assert_eq!(realtime_json(&mut peer)["revision"], 1);
    // Another process commits through its own handle: the row appears in the shared
    // database but this process's in-memory watch never fires.
    let changes = f.host().releases().subscribe();
    let db = f.host().database();
    db.write(|c| {
        c.execute(
            "INSERT INTO pack_releases (server_id,revision,minimum_revision,display_version,format,sha256,size,published_at) VALUES (?,2,2,'2','zip',?,?,?)",
            rusqlite::params![
                db.server_id(),
                first.artifact().sha256().to_hex(),
                first.artifact().size() as i64,
                db.clock().millis()
            ],
        )?;
        Ok(())
    })
    .unwrap();
    assert!(!changes.has_changed().unwrap());
    peer.get_mut()
        .sock
        .set_read_timeout(Some(Duration::from_secs(12)))
        .unwrap();
    // Delivered on the next heartbeat, before the heartbeat notice itself.
    assert_eq!(
        realtime_json(&mut peer),
        serde_json::json!({"type":"pack_update", "revision":2, "minimum_revision":2, "status":"update_required"})
    );
    assert_eq!(
        realtime_json(&mut peer),
        serde_json::json!({"type":"heartbeat"})
    );
}

#[test]
fn realtime_upgraded_socket_still_counts_against_listener_connection_limit() {
    let f = Fixture::new("minecraft-or-password", 1, 4096, 10);
    let owner = f.owner();
    let session = f.password_session(&owner);
    let mut peer = f.websocket(&f.direct_ticket(&session), 0).unwrap();
    realtime_json(&mut peer);
    assert!(
        connect(f.address, true).is_err(),
        "upgrade bypassed the listener connection cap"
    );
    peer.close(None).unwrap();
    assert!(matches!(peer.read().unwrap(), Message::Close(_)));
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if connect(f.address, true).is_ok() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "closed upgrade retained listener capacity"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn realtime_ping_and_unsolicited_pong_floods_close_with_policy_violation() {
    let f = Fixture::new("minecraft-or-password", 8, 4096, 10);
    let owner = f.owner();
    let session = f.password_session(&owner);
    for message in [Message::Ping(vec![1]), Message::Pong(vec![1])] {
        let mut peer = f.websocket(&f.ticket(&session, true), 0).unwrap();
        realtime_json(&mut peer);
        for _ in 0..16 {
            peer.write(message.clone()).unwrap();
        }
        peer.flush().unwrap();
        let mut pongs = 0;
        loop {
            match peer.read().unwrap() {
                Message::Pong(_) => {
                    pongs += 1;
                    assert!(pongs <= 9);
                }
                Message::Close(Some(frame)) => {
                    assert_eq!(frame.code, CloseCode::Policy);
                    break;
                }
                other => panic!("unexpected flood response: {other:?}"),
            }
        }
    }
}

#[test]
fn realtime_two_ping_floods_do_not_starve_http_requests() {
    use std::sync::{
        Barrier,
        atomic::{AtomicUsize, Ordering},
    };
    let f = Fixture::new("minecraft-or-password", 8, 4096, 10);
    let owner = f.owner();
    let session = f.password_session(&owner);
    let start = Arc::new(Barrier::new(3));
    let finished = Arc::new(AtomicUsize::new(0));
    let mut floods = Vec::new();
    for _ in 0..2 {
        let mut peer = f.websocket(&f.ticket(&session, true), 0).unwrap();
        realtime_json(&mut peer);
        let start = start.clone();
        let finished = finished.clone();
        floods.push(std::thread::spawn(move || {
            start.wait();
            for _ in 0..16 {
                peer.send(Message::Ping(vec![1])).unwrap();
                match peer.read().unwrap() {
                    Message::Pong(_) => std::thread::sleep(Duration::from_millis(10)),
                    Message::Close(Some(frame)) => {
                        assert_eq!(frame.code, CloseCode::Policy);
                        finished.fetch_add(1, Ordering::SeqCst);
                        return;
                    }
                    other => panic!("unexpected flood response: {other:?}"),
                }
            }
            // The codec may flush one queued Pong with the policy close.
            realtime_close(&mut peer, CloseCode::Policy);
            finished.fetch_add(1, Ordering::SeqCst);
        }));
    }
    start.wait();
    let mut during_flood = 0;
    for _ in 0..20 {
        let active = finished.load(Ordering::SeqCst) < 2;
        let reply = f.request("GET", "/api/v2/pack/status?installed=0", b"", None);
        assert_eq!(
            reply.status, 200,
            "control traffic consumed HTTP blocking capacity"
        );
        if active {
            during_flood += 1;
        }
    }
    for flood in floods {
        flood.join().unwrap();
    }
    assert!(
        during_flood > 0,
        "fixture did not overlap flood and HTTP traffic"
    );
}

fn metrics_websocket(f: &Fixture, token: &str) -> Result<RealtimePeer, u16> {
    let mut request = format!(
        "wss://localhost:{}/api/v3/manage/metrics/stream",
        f.address.port()
    )
    .into_client_request()
    .unwrap();
    request
        .headers_mut()
        .insert("authorization", format!("Bearer {token}").parse().unwrap());
    match tungstenite::client(request, f.stream()) {
        Ok((peer, reply)) => {
            assert_eq!(reply.status().as_u16(), 101);
            assert!(
                !reply
                    .headers()
                    .keys()
                    .any(|name| name.as_str().starts_with("access-control-"))
            );
            Ok(peer)
        }
        Err(tungstenite::HandshakeError::Failure(tungstenite::Error::Http(reply))) => {
            Err(reply.status().as_u16())
        }
        Err(error) => panic!("owned metrics upgrade failed: {error}"),
    }
}
fn metrics_activity(f: &Fixture, name: &str) {
    f.host()
        .database()
        .write(|c| {
            let id: i64 = c.query_row(
                "SELECT COALESCE(MAX(id),0)+1 FROM metrics_activity",
                [],
                |r| r.get(0),
            )?;
            let at = f.host().database().clock().millis();
            c.execute(
                "INSERT INTO metrics_activity(id,at,generation,name,kind,bytes) VALUES(?,?,'00112233445566778899aabbccddeeff',?,'join',100)",
                rusqlite::params![id, at, name],
            )?;
            c.execute(
                "UPDATE metrics_status SET storage_bytes=storage_bytes+100 WHERE singleton=1",
                [],
            )?;
            Ok(())
        })
        .unwrap();
}
fn metrics_policy_sample_period(f: &Fixture, token: &str, seconds: u64) {
    let mut settings =
        serde_json::to_value(vesta_control::config::MetricsSettings::default()).unwrap();
    settings["sample_seconds"] = serde_json::json!(seconds);
    let body = serde_json::to_vec(&serde_json::json!({
        "expected_revision": null,
        "settings": settings
    }))
    .unwrap();
    let reply = f.request("PUT", "/api/v3/manage/metrics/policy", &body, Some(token));
    assert_eq!(
        reply.status,
        200,
        "{}",
        String::from_utf8_lossy(&reply.body)
    );
}
fn assert_no_player_event(peer: &mut RealtimePeer, name: &str) {
    peer.get_mut()
        .sock
        .set_read_timeout(Some(Duration::from_millis(500)))
        .unwrap();
    let deadline = Instant::now() + Duration::from_millis(500);
    while Instant::now() < deadline {
        match peer.read() {
            Ok(Message::Text(text)) => {
                let notice: serde_json::Value = serde_json::from_str(&text).unwrap();
                assert_ne!(notice["type"], "metrics.player_activity", "{notice}");
                assert!(!notice.to_string().contains(name), "{notice}");
            }
            Ok(Message::Ping(_)) => peer.flush().unwrap(),
            Ok(other) => panic!("unexpected metrics notice during identity check: {other:?}"),
            Err(tungstenite::Error::Io(error))
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                ) =>
            {
                break;
            }
            Err(error) => panic!("metrics identity check failed: {error}"),
        }
    }
    peer.get_mut()
        .sock
        .set_read_timeout(Some(Duration::from_secs(15)))
        .unwrap();
}

pub(super) fn console_websocket(f: &Fixture, token: &str) -> Result<RealtimePeer, u16> {
    let mut request = format!(
        "wss://localhost:{}/api/v3/manage/console/stream",
        f.address.port()
    )
    .into_client_request()
    .unwrap();
    request
        .headers_mut()
        .insert("authorization", format!("Bearer {token}").parse().unwrap());
    match tungstenite::client(request, f.stream()) {
        Ok((peer, reply)) => {
            assert_eq!(reply.status().as_u16(), 101);
            Ok(peer)
        }
        Err(tungstenite::HandshakeError::Failure(tungstenite::Error::Http(reply))) => {
            Err(reply.status().as_u16())
        }
        Err(error) => panic!("owned console upgrade failed: {error}"),
    }
}

#[test]
fn console_stream_checks_capability_bounds_connections_and_rechecks_revocation() {
    let f = Fixture::new("minecraft", 8, 4096, 3);
    let pack = f.device_session();
    assert_eq!(console_websocket(&f, &pack).unwrap_err(), 403);
    assert_eq!(console_websocket(&f, &"a".repeat(43)).unwrap_err(), 401);
    let (device, key) = enroll_management(&f, vesta_control::identity::BuiltinRole::Owner);
    let token = management_session(&f, &device, &key);
    let mut first = console_websocket(&f, &token).unwrap();
    let mut second = console_websocket(&f, &token).unwrap();
    for peer in [&mut first, &mut second] {
        let hello = realtime_json(peer);
        assert_eq!(hello["type"], "console.available");
        assert!(hello.to_string().len() < 1024);
        assert!(hello.get("text").is_none());
        assert!(!hello.to_string().contains(&token));
    }
    assert_eq!(console_websocket(&f, &token).unwrap_err(), 503);
    f.host().management().revoke_local(&device).unwrap();
    realtime_close(&mut first, CloseCode::Policy);
    realtime_close(&mut second, CloseCode::Policy);
}

#[test]
fn metrics_stream_snapshots_samples_latest_activity_and_rechecks_both_capabilities() {
    use vesta_control::identity::BuiltinRole;

    let f = Fixture::new("minecraft", 8, 4096, 5);
    let (device, key) = enroll_management(&f, BuiltinRole::Owner);
    let token = management_session(&f, &device, &key);
    let (moderator_device, moderator_key) = enroll_management(&f, BuiltinRole::Moderator);
    let moderator = management_session(&f, &moderator_device, &moderator_key);
    assert_eq!(metrics_websocket(&f, &moderator).err(), Some(403));

    metrics_policy_sample_period(&f, &token, 5);
    let mut first = metrics_websocket(&f, &token).unwrap();
    let mut second = metrics_websocket(&f, &token).unwrap();
    for peer in [&mut first, &mut second] {
        peer.get_mut()
            .sock
            .set_read_timeout(Some(Duration::from_secs(15)))
            .unwrap();
        let snapshot = realtime_json(peer);
        assert_eq!(snapshot["type"], "metrics.snapshot");
        assert_eq!(snapshot["api_version"], "3");
        assert!(snapshot["columns"]["timestamps"].is_array());
        assert!(snapshot["columns"]["series"]["tps"]["avg"].is_array());
        assert!(snapshot["status"].is_object());
        assert!(snapshot["cursor"].as_i64().is_some());
        let status = realtime_json(peer);
        assert_eq!(status["type"], "status");
        assert_eq!(status["data"]["state"], "stopped");
        assert!(status.to_string().len() <= 4096);
    }
    assert_eq!(metrics_websocket(&f, &token).err(), Some(503));
    second.close(None).unwrap();
    // Additive live-resource frames may already be buffered before our close.
    let mut closed = false;
    for _ in 0..8 {
        match second.read().unwrap() {
            Message::Close(_) => { closed = true; break; }
            Message::Text(text) => {
                let notice: serde_json::Value = serde_json::from_str(&text).unwrap();
                assert!(matches!(notice["type"].as_str(), Some("resources.changed" | "status" | "metrics.sample" | "heartbeat")));
            }
            Message::Ping(_) => second.flush().unwrap(),
            other => panic!("unexpected close-handshake frame {other:?}"),
        }
    }
    assert!(closed);

    metrics_activity(&f, "VisiblePlayer");
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut saw_sample = false;
    let mut saw_player = false;
    while !saw_sample || !saw_player {
        assert!(
            Instant::now() < deadline,
            "metrics publication did not arrive"
        );
        let notice = realtime_json(&mut first);
        match notice["type"].as_str().unwrap_or_default() {
            "status" => assert!(notice["data"].is_object()),
            "resources.changed" => assert!(notice["resources"].is_object()),
            "metrics.gap" => assert_eq!(notice["mode"], "latest_only"),
            "metrics.sample" => {
                saw_sample = true;
                assert!(notice["columns"]["timestamps"].is_array());
            }
            "metrics.player_activity" => {
                saw_player = true;
                assert_eq!(notice["event"]["name"], "VisiblePlayer");
                assert_eq!(notice["event"]["kind"], "join");
            }
            kind => panic!("unexpected metrics notice {kind}: {notice}"),
        }
    }

    f.host()
        .database()
        .write(|c| {
            let principal: String = c.query_row(
                "SELECT principal_id FROM devices WHERE id=?",
                [&device],
                |r| r.get(0),
            )?;
            c.execute(
                "INSERT INTO principal_overrides(id,principal_id,capability,scope,effect,granted_by,created_at) VALUES('metrics-player-view-deny',?,'players.view','*','deny','fixture',?)",
                rusqlite::params![principal, f.host().database().clock().millis()],
            )?;
            Ok(())
        })
        .unwrap();
    metrics_activity(&f, "HiddenPlayer");
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        assert!(
            Instant::now() < deadline,
            "metrics publication after revocation did not arrive"
        );
        let notice = realtime_json(&mut first);
        match notice["type"].as_str().unwrap_or_default() {
            "status" => assert!(notice["data"].is_object()),
            "resources.changed" => assert!(notice["resources"].is_object()),
            "metrics.gap" => assert_eq!(notice["mode"], "latest_only"),
            "metrics.sample" => break,
            kind => panic!("unexpected metrics notice after PlayersView denial {kind}: {notice}"),
        }
    }
    assert_no_player_event(&mut first, "HiddenPlayer");

    f.host().database().write(|c| {
        let principal: String = c.query_row("SELECT principal_id FROM devices WHERE id=?", [&device], |r| r.get(0))?;
        c.execute("INSERT INTO principal_overrides(id,principal_id,capability,scope,effect,granted_by,created_at) VALUES('metrics-view-deny',?,'metrics.view','*','deny','fixture',?)", rusqlite::params![principal, f.host().database().clock().millis()])?;
        Ok(())
    }).unwrap();
    realtime_close(&mut first, CloseCode::Policy);
    // The second connection completed its normal close handshake above.
}

#[test]
fn status_stream_pushes_lifecycle_changes_without_telemetry_and_rechecks_status_view() {
    let f = Fixture::inert();
    let (device, key) = enroll_management(&f, vesta_control::identity::BuiltinRole::Owner);
    let token = management_session(&f, &device, &key);
    let settings = vesta_control::config::MetricsSettings {
        enabled: false,
        ..Default::default()
    };
    assert_eq!(
        f.request(
            "PUT",
            "/api/v3/manage/metrics/policy",
            &serde_json::to_vec(&serde_json::json!({"expected_revision":null,"settings":settings}))
                .unwrap(),
            Some(&token)
        )
        .status,
        200
    );
    let mut peer = metrics_websocket(&f, &token).unwrap();
    assert_eq!(realtime_json(&mut peer)["type"], "metrics.snapshot");
    let first = realtime_json(&mut peer);
    assert_eq!(first["type"], "status");
    assert_eq!(first["data"]["state"], "stopped");
    let http = f.request("GET", "/api/v3/manage/server/status", b"", Some(&token));
    assert_eq!(first["data"], http.json()["data"]);
    assert_eq!(
        first["etag"].as_str(),
        http.headers.get("etag").map(String::as_str)
    );
    let generation = management_start_inert(&f, &token);
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        assert!(Instant::now() < deadline, "running status was not pushed");
        let notice = realtime_json(&mut peer);
        if notice["type"] == "status" && notice["data"]["state"] == "running" {
            assert_eq!(notice["data"]["generation"], generation);
            assert_ne!(notice["etag"], first["etag"]);
            assert!(notice.to_string().len() <= 4096);
            break;
        }
    }
    f.host().database().write(|c| {
        let principal:String=c.query_row("SELECT principal_id FROM devices WHERE id=?",[&device],|r|r.get(0))?;
        c.execute("INSERT INTO principal_overrides(id,principal_id,capability,scope,effect,granted_by,created_at) VALUES('status-deny',?,'status.view','*','deny','fixture',0)",[principal])?;
        Ok(())
    }).unwrap();
    peer.close(None).unwrap();
    let _ = peer.read();
    // A fresh metrics connection is permitted but must withhold status entirely.
    let mut peer = metrics_websocket(&f, &token).unwrap();
    assert_eq!(realtime_json(&mut peer)["type"], "metrics.snapshot");
    peer.get_mut()
        .sock
        .set_read_timeout(Some(Duration::from_millis(800)))
        .unwrap();
    match peer.read() {
        Err(tungstenite::Error::Io(e))
            if matches!(
                e.kind(),
                std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
            ) => {}
        other => panic!("metrics-only subscriber received unexpected status: {other:?}"),
    }
    peer.get_mut()
        .sock
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    f.host().management().revoke_local(&device).unwrap();
    realtime_close(&mut peer, CloseCode::Policy);
}

#[test]
fn integrated_12k_roster_status_metrics_network_and_resource_events() {
    let f = Fixture::inert();
    let names = (0..12_000).map(|i| format!("Player{i:05}")).collect::<Vec<_>>();
    std::fs::write(f.temp.path().join("game/tls-inert-roster"), names.join(", ")).unwrap();
    let (device, key) = enroll_management(&f, vesta_control::identity::BuiltinRole::Owner);
    let token = management_session(&f, &device, &key);
    metrics_policy_sample_period(&f, &token, 5);
    let generation = management_start_inert(&f, &token);
    let seed = f.request("GET", "/api/v3/manage/players", b"", Some(&token));
    assert_eq!(seed.json()["data"]["player_count"], 12_000, "{}", seed.json());
    let base = seed.json()["data"]["revision"].clone();
    let mut peer = metrics_websocket(&f, &token).unwrap();
    let until = Instant::now() + Duration::from_secs(10);
    loop {
        assert!(Instant::now() < until);
        if realtime_json(&mut peer)["type"] == "resources.changed" { break; }
    }
    // A second administrator changes branding while this subscriber stays open.
    let (other_device, other_key) = enroll_management(&f, vesta_control::identity::BuiltinRole::Owner);
    let other = management_session(&f, &other_device, &other_key);
    let body = serde_json::json!({"expected_revision":"0","name":"Live edit"});
    let patch = super::management_permissions::send(&f,"PATCH","server",&body,Some(&other),Some("live-branding-editor-001"));
    assert_eq!(patch.status, 200);
    let editor = patch.json()["data"]["editor"].clone();
    assert!(editor["display_name"].as_str().is_some_and(|n| !n.is_empty()), "{editor}");
    assert!(editor["at_ms"].as_i64().unwrap() > 0);
    let conflict = super::management_permissions::send(&f,"PATCH","server",&body,Some(&token),Some("live-branding-losing-001"));
    assert_eq!(conflict.status,409);
    assert_eq!(conflict.json()["error"]["current"]["editor"],editor);
    assert_eq!(conflict.json()["error"]["current"]["name"],"Live edit");
    loop {
        assert!(Instant::now() < until);
        let notice = realtime_json(&mut peer);
        if notice["type"] == "resources.changed" && notice["resources"]["branding"] == editor {break;}
    }
    // Real stdin -> captured console -> roster reconciliation -> stream delta.
    let event_start = Instant::now();
    for (i, text) in ["Player00000 left the game", "DeltaGuest joined the game"].iter().enumerate() {
        let reply = management_mutation(&f,&token,"console/command",&format!("load-delta-command-{i:04}"),&serde_json::json!({"expected_generation":generation,"line":format!("owned-roster-event {text}")}));
        assert_eq!(reply.status,202);
    }
    let mut revision = base;
    let mut changes = Vec::new();
    let mut delta_bytes = 0;
    let mut frames = 0;
    while changes.len() < 2 {
        assert!(event_start.elapsed() < Duration::from_secs(10));
        let notice = realtime_json(&mut peer);
        if notice["type"] == "players.delta" {
            assert_eq!(notice["data"]["base_revision"], revision);
            revision = notice["data"]["revision"].clone();
            delta_bytes += notice.to_string().len();
            frames += 1;
            changes.extend(notice["data"]["changes"].as_array().unwrap().clone());
        }
    }
    assert!(changes.iter().any(|c|c["name"]=="DeltaGuest" && c["kind"]=="join"));
    assert!(changes.iter().any(|c|c["name"]=="Player00000" && c["kind"]=="leave"));
    let delta_ms = event_start.elapsed().as_secs_f64()*1000.;
    let start = Instant::now();
    let flood = management_mutation(&f,&token,"console/command","load-roster-flood-0001",&serde_json::json!({"expected_generation":generation,"line":"owned-roster-flood"}));
    assert_eq!(flood.status,202);
    let mut latencies = std::collections::BTreeMap::<&str,Vec<f64>>::new();
    let mut bytes = 0;
    let mut memory: Option<f64> = None;
    let mut cpu: Option<f64> = None;
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis();
    let history = format!("/api/v3/manage/metrics?series=agent_cpu_percent&from={}&to={now}&resolution=raw", now - 60_000);
    for _ in 0..20 {
        for (name,path) in [("status","/api/v3/manage/server/status"),("public_status","/api/v3/status"),("roster_page","/api/v3/manage/players?limit=200"),("metrics_current","/api/v3/manage/metrics/current"),("metrics_history",history.as_str())] {
            let at = Instant::now();
            let reply = f.request("GET",path,b"",Some(&token));
            assert_eq!(reply.status,200,"{name}: {}",reply.json());
            latencies.entry(name).or_default().push(at.elapsed().as_secs_f64()*1000.);
            bytes += reply.body.len();
            let value = reply.json();
            if name=="status" {assert!((12_000..=12_001).contains(&value["data"]["player_count"].as_u64().unwrap()));}
            if name=="roster_page" {assert_eq!(value["data"]["items"].as_array().unwrap().len(),200);}
            if name=="metrics_current" {
                if let Some(v)=value["data"]["sample"]["values"]["agent_memory_bytes"].as_f64(){memory=Some(v);}
                if let Some(v)=value["data"]["sample"]["values"]["agent_cpu_percent"].as_f64(){cpu=Some(v);}
            }
        }
    }
    let elapsed=start.elapsed().as_secs_f64();
    let mut churn = 0;
    let mut churn_bytes = 0;
    let mut churn_frames = 0;
    while churn < 120 {
        assert!(start.elapsed()<Duration::from_secs(15));
        let notice = realtime_json(&mut peer);
        if notice["type"]=="players.delta" {
            assert_eq!(notice["data"]["base_revision"],revision);
            revision=notice["data"]["revision"].clone();
            churn+=notice["data"]["changes"].as_array().unwrap().len();
            churn_bytes+=notice.to_string().len(); churn_frames+=1;
        }
    }
    let churn_seconds=start.elapsed().as_secs_f64();
    let summary=latencies.into_iter().map(|(name,mut values)|{values.sort_by(f64::total_cmp);(name,serde_json::json!({"p50_ms":values[10],"p95_ms":values[18],"max_ms":values[19]}))}).collect::<std::collections::BTreeMap<_,_>>();
    println!("INTEGRATED_SCALE {}",serde_json::json!({"players":12000,"requests":100,"wall_seconds":elapsed,"requests_per_second":100./elapsed,"response_body_bytes":bytes,"seed_bytes":seed.body.len(),"delta_frames":frames,"delta_bytes":delta_bytes,"delta_delivery_ms":delta_ms,"churn_events":churn,"churn_frames":churn_frames,"churn_bytes":churn_bytes,"churn_events_per_second":churn as f64/churn_seconds,"agent_memory_bytes":memory,"agent_cpu_percent":cpu,"latency":summary}));
    peer.close(None).unwrap();
}
