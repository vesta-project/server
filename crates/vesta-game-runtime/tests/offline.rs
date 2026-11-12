//! Real loopback protocol checks; no Minecraft, public bind, or external network.
use std::{
    io::{Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    time::{Duration, Instant},
};
use vesta_game_runtime::offline::{Configuration, Responder};

fn config(wake: bool) -> Configuration {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    Configuration {address,status_json:r#"{"version":{"name":"Vesta","protocol":767},"players":{"online":0,"max":0},"description":{"text":"Offline"}}"#.into(),disconnect:"Starting; reconnect shortly".into(),wake}
}
fn varint(mut n: usize) -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        let b = (n & 127) as u8;
        n >>= 7;
        out.push(b | if n == 0 { 0 } else { 128 });
        if n == 0 {
            break;
        }
    }
    out
}
fn send(s: &mut TcpStream, p: &[u8]) {
    s.write_all(&varint(p.len())).unwrap();
    s.write_all(p).unwrap();
}
fn read(s: &mut TcpStream) -> Vec<u8> {
    let mut n = 0;
    for i in 0..5 {
        let mut b = [0];
        s.read_exact(&mut b).unwrap();
        n |= ((b[0] & 127) as usize) << (7 * i);
        if b[0] & 128 == 0 {
            let mut data = vec![0; n];
            s.read_exact(&mut data).unwrap();
            return data;
        }
    }
    panic!("invalid owned reply")
}
fn peer(address: SocketAddr, next: u8) -> TcpStream {
    let mut s = TcpStream::connect(address).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    let mut handshake = vec![0];
    handshake.extend(varint(767));
    handshake.push(9);
    handshake.extend(b"localhost");
    handshake.extend(address.port().to_be_bytes());
    handshake.push(next);
    send(&mut s, &handshake);
    s
}
fn login(address: SocketAddr) -> Vec<u8> {
    let mut s = peer(address, 2);
    let mut p = vec![0, 4];
    p.extend(b"Alex");
    p.extend([0; 16]);
    send(&mut s, &p);
    read(&mut s)
}
fn wakes(r: &Responder) -> Vec<std::net::IpAddr> {
    let end = Instant::now() + Duration::from_secs(1);
    loop {
        let v = r.take_wakes();
        if !v.is_empty() {
            return v;
        }
        if Instant::now() >= end {
            return v;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}
#[test]
fn offline_ping_echoes_payload_never_wakes_and_does_not_consume_join_wake_budget() {
    let c = config(true);
    let mut r = Responder::bind(c.clone()).unwrap();
    let mut s = peer(c.address, 1);
    send(&mut s, &[0]);
    let response = read(&mut s);
    assert_eq!(response[0], 0);
    assert!(String::from_utf8_lossy(&response).contains("Offline"));
    let mut ping = vec![1];
    ping.extend(12345678i64.to_be_bytes());
    send(&mut s, &ping);
    assert_eq!(read(&mut s), ping);
    assert!(r.take_wakes().is_empty());
    assert!(String::from_utf8_lossy(&login(c.address)).contains("reconnect"));
    assert_eq!(wakes(&r), vec![c.address.ip()]);
    login(c.address);
    assert!(r.take_wakes().is_empty());
    r.close().unwrap();
    assert!(TcpListener::bind(c.address).is_ok());
}
#[test]
fn offline_malformed_or_incomplete_login_never_requests_wake() {
    let c = config(true);
    let mut r = Responder::bind(c.clone()).unwrap();
    for packet in [
        vec![0, 1, b'!'],
        vec![0, 0],
        vec![0, 4, b'A'],
        vec![0, 4, b'A', b'l', b'e', b'x', 0],
    ] {
        let mut s = peer(c.address, 2);
        send(&mut s, &packet);
        let mut b = [0];
        let _ = s.read(&mut b);
    }
    let s = peer(c.address, 2);
    drop(s);
    assert!(r.take_wakes().is_empty());
    r.close().unwrap();
}
#[test]
fn offline_disabled_wake_and_invalid_public_player_samples_are_refused() {
    let mut c = config(false);
    let mut r = Responder::bind(c.clone()).unwrap();
    login(c.address);
    assert!(r.take_wakes().is_empty());
    r.close().unwrap();
    for players in [
        r#"{"online":1,"max":20}"#,
        r#"{"online":0,"max":20,"sample":[{"name":"private"}]}"#,
    ] {
        c.status_json = format!(
            r#"{{"version":{{"name":"Vesta","protocol":767}},"players":{players},"description":{{"text":"Offline"}}}}"#
        );
        assert!(c.validate().is_err());
    }
}
#[test]
fn offline_partial_clients_have_a_deadline_and_release_all_port_ownership() {
    let c = config(true);
    let mut r = Responder::bind(c.clone()).unwrap();
    let mut clients = Vec::new();
    for _ in 0..8 {
        let mut s = TcpStream::connect(c.address).unwrap();
        s.write_all(&[128]).unwrap();
        clients.push(s);
    }
    std::thread::sleep(Duration::from_millis(40));
    let at = Instant::now();
    r.close().unwrap();
    assert!(at.elapsed() < Duration::from_secs(2));
    assert!(TcpListener::bind(c.address).is_ok());
    drop(clients);
}
#[test]
fn offline_presentation_updates_keep_listener_and_source_wake_cooldown() {
    let c = config(true);
    let mut r = Responder::bind(c.clone()).unwrap();
    login(c.address);
    assert_eq!(wakes(&r).len(), 1);
    let mut changed = c;
    changed.disconnect = "Updated reconnect text".into();
    changed.status_json = changed.status_json.replace("Offline", "Updated");
    r.update(changed.clone()).unwrap();
    assert!(
        std::net::TcpListener::bind(changed.address).is_err(),
        "same-address updates keep the actual listener"
    );
    assert!(String::from_utf8_lossy(&login(changed.address)).contains("Updated"));
    assert!(
        r.take_wakes().is_empty(),
        "presentation changes must not reset source cooldown"
    );
    let mut moved = changed;
    moved
        .address
        .set_port(moved.address.port().saturating_add(1));
    assert!(r.update(moved).is_err());
    r.close().unwrap();
}
