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
    match peer.read().unwrap() {
        Message::Text(text) => serde_json::from_str(&text).unwrap(),
        other => panic!("expected JSON notice, got {other:?}"),
    }
}
fn realtime_close(peer: &mut RealtimePeer, code: CloseCode) {
    match peer.read().unwrap() {
        Message::Close(Some(frame)) => assert_eq!(frame.code, code),
        other => panic!("expected close, got {other:?}"),
    }
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
    let original = f.password_session(&owner);
    let grant = f
        .host()
        .pack_sessions()
        .authenticate(&original, Instant::now(), &f.host().pack_policy().unwrap())
        .unwrap()
        .grant();
    let (session, _) = f
        .host()
        .pack_sessions()
        .issue(grant, Instant::now() - Duration::from_millis(3_598_500))
        .unwrap();
    let mut peer = f.websocket(&f.direct_ticket(session.expose()), 0).unwrap();
    assert_eq!(
        realtime_json(&mut peer),
        serde_json::json!({"type":"hello", "revision":0, "minimum_revision":0, "status":null})
    );
    let start = Instant::now();
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
    let (session, _) = f
        .host()
        .pack_sessions()
        .issue(
            vesta_control::pack_access::PackGrant::Password(PasswordRevision::new(1).unwrap()),
            Instant::now(),
        )
        .unwrap();
    let mut peer = f.websocket(&f.direct_ticket(session.expose()), 0).unwrap();
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
    let mut peers = Vec::new();
    for _ in 0..32 {
        let (session, _) = f
            .host()
            .pack_sessions()
            .issue(
                vesta_control::pack_access::PackGrant::Password(PasswordRevision::new(1).unwrap()),
                Instant::now(),
            )
            .unwrap();
        let mut peer = f.websocket(&f.direct_ticket(session.expose()), 0).unwrap();
        realtime_json(&mut peer);
        peers.push(peer);
    }
    let (session, _) = f
        .host()
        .pack_sessions()
        .issue(
            vesta_control::pack_access::PackGrant::Password(PasswordRevision::new(1).unwrap()),
            Instant::now(),
        )
        .unwrap();
    assert_eq!(
        f.websocket(&f.direct_ticket(session.expose()), 0).err(),
        Some(503)
    );
    f.publish(
        &owner,
        UpdatePolicy::Optional,
        b"full-budget broadcast",
        "1",
    );
    for peer in &mut peers {
        assert_eq!(
            realtime_json(peer),
            serde_json::json!({"type":"pack_update", "revision":1, "minimum_revision":1, "status":"update_required"})
        );
    }
    f.finish();
    for peer in &mut peers {
        realtime_close(peer, CloseCode::Away);
    }
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
