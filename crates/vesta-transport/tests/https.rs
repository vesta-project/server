//! All sockets, state and launch placeholders are owned test fixtures. No game is
//! started and no Mojang/provider request is made: completion uses an unknown ID.
//! Committed PEM material is test-only, never used as production trust or defaults.
use rustls::{ClientConfig, ClientConnection, RootCertStore, StreamOwned};
use rustls_pki_types::{ServerName, pem::PemObject};
use std::{
    collections::BTreeMap,
    io::{BufRead, BufReader, Read, Write},
    net::{SocketAddr, TcpStream},
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};
use tempfile::TempDir;
use vesta_control::{
    config::{HostConfiguration, ListenerDescription},
    host::{Host, HostShutdown},
    identity::{DeviceId, PasswordRevision, PrincipalId, Subject, password::SecretPassword},
    packs::UpdatePolicy,
};
use vesta_protocol::discovery::{self, DiscoveryRequest};
use vesta_state::{StateDatabase, StateDirectory, SystemClock};
use vesta_transport::{Transport, TransportShutdown};

const CERT: &[u8] = include_bytes!("fixtures/test-only-server.pem");
const ROTATED_CERT: &[u8] = include_bytes!("fixtures/test-only-rotated-server.pem");
const ROTATED_KEY: &[u8] = include_bytes!("fixtures/test-only-mismatched-key.pem");
const KEY: &[u8] = include_bytes!("fixtures/test-only-server-key.pem");
const CA: &[u8] = include_bytes!("fixtures/test-only-ca.pem");
const PASSWORD: &str = "transport test password only";
type TlsStream = StreamOwned<ClientConnection, TcpStream>;

struct Fixture {
    temp: TempDir,
    host: Option<Host>,
    transport: Option<Transport>,
    address: SocketAddr,
}
impl Fixture {
    fn new(mode: &str, connections: u16, max_body: u32, timeout: u16) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let base = temp.path();
        for dir in ["game/config", "bin", "private"] {
            std::fs::create_dir_all(base.join(dir)).unwrap();
        }
        std::fs::write(base.join("bin/inert-placeholder"), b"").unwrap();
        std::fs::write(base.join("private/launch.toml"), "[minecraft]\nworking-directory = 'game'\n[launch]\nkind = 'native'\nexecutable = '../bin/inert-placeholder'\nstop = 'console-stop'\n").unwrap();
        std::fs::write(base.join("private/cert.pem"), CERT).unwrap();
        std::fs::write(base.join("private/key.pem"), KEY).unwrap();
        let document = format!(
            "schema-version = 1\n[paths]\nstate = 'private-state'\ngame = 'game'\nscan-roots = ['game/config']\nlaunch-definition = 'private/launch.toml'\n[pack]\nname = 'Hosted'\nversion = '1.0.0'\nrevision = 1\nminimum-revision = 1\nminecraft-version = '1.21.1'\n[pack-access]\nmode = '{mode}'\n[listener]\nmode = 'tls'\nbind = '127.0.0.1'\nport = 25567\ncertificate = 'private/cert.pem'\nprivate-key = 'private/key.pem'\nmax-connections = {connections}\nmax-request-bytes = {max_body}\nrequest-timeout-seconds = {timeout}\n"
        );
        let config =
            HostConfiguration::parse_toml(document.as_bytes(), base, Path::new("host.toml"))
                .unwrap();
        let ListenerDescription::Tls(listener) = config.listener() else {
            panic!("TLS fixture");
        };
        let listener = listener.on_ephemeral_port();
        let host = Host::open(config, Arc::new(SystemClock)).unwrap();
        let transport = Transport::start(host.network_handle(), &listener).unwrap();
        let address = transport.local_addr();
        assert!(address.ip().is_loopback());
        assert_ne!(address.port(), 0);
        Self {
            temp,
            host: Some(host),
            transport: Some(transport),
            address,
        }
    }
    fn host(&self) -> &Host {
        self.host.as_ref().unwrap()
    }
    fn stream(&self) -> TlsStream {
        connect(self.address, true).unwrap()
    }
    fn request(&self, method: &str, path: &str, body: &[u8], session: Option<&str>) -> WireReply {
        let mut stream = self.stream();
        let authorization = session
            .map(|s| format!("Authorization: Bearer {s}\r\n"))
            .unwrap_or_default();
        write!(stream, "{method} {path} HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\nOrigin: https://untrusted.invalid\r\n{authorization}\r\n", body.len()).unwrap();
        stream.write_all(body).unwrap();
        stream.flush().unwrap();
        read_reply(stream).unwrap()
    }
    fn owner(&self) -> Subject {
        let db = self.host().database();
        let principal = PrincipalId::parse("aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa").unwrap();
        let device = DeviceId::parse("bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb").unwrap();
        db.write(|c| {
            let now = db.clock().millis();
            c.execute("INSERT INTO principals (id,server_id,display_name,status,created_at,updated_at) VALUES (?,?,'Fixture','active',?,?)", rusqlite::params![principal.as_str(),db.server_id(),now,now])?;
            c.execute("INSERT INTO devices (id,server_id,principal_id,label,public_key_sha256,created_at) VALUES (?,?,?,'Fixture',?,?)", rusqlite::params![device.as_str(),db.server_id(),principal.as_str(),"0".repeat(64),now])?;
            c.execute("INSERT INTO role_assignments (id,principal_id,role_id,granted_by,created_at) VALUES ('cccccccc-cccc-4ccc-8ccc-cccccccccccc',?,'owner','fixture',?)", rusqlite::params![principal.as_str(),now])?;
            Ok(())
        }).unwrap();
        Subject { principal, device }
    }
    fn publish(
        &self,
        owner: &Subject,
        policy: UpdatePolicy,
        bytes: &[u8],
        version: &str,
    ) -> vesta_control::packs::PackRelease {
        let host = self.host();
        std::fs::write(self.temp.path().join("game/config/pack.zip"), bytes).unwrap();
        let archive = host.pack_preparations()[0]
            .archive(&vesta_control::files::RelativePath::parse("pack.zip").unwrap())
            .unwrap();
        let previous = host
            .releases()
            .current()
            .unwrap()
            .map_or(0, |r| r.revision());
        let plan = host.releases().plan(owner, previous, policy).unwrap();
        host.releases()
            .publish_archive(owner, &plan, &archive, version, false)
            .unwrap()
    }
    fn finish(&mut self) {
        if let Some(transport) = self.transport.take() {
            match transport.shutdown(Duration::from_secs(5)).unwrap() {
                TransportShutdown::Complete => {}
                TransportShutdown::TimedOut(_) => panic!("transport timeout"),
            }
        }
        if let Some(host) = self.host.take() {
            assert!(matches!(
                host.shutdown(Duration::from_secs(5)).unwrap(),
                HostShutdown::Complete
            ));
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.finish();
    }
}

fn connect(address: SocketAddr, trusted: bool) -> std::io::Result<TlsStream> {
    let socket = TcpStream::connect_timeout(&address, Duration::from_secs(3))?;
    socket.set_read_timeout(Some(Duration::from_secs(3)))?;
    socket.set_write_timeout(Some(Duration::from_secs(3)))?;
    let mut roots = RootCertStore::empty();
    if trusted {
        roots
            .add(rustls_pki_types::CertificateDer::from_pem_slice(CA).unwrap())
            .unwrap();
        roots
            .add(rustls_pki_types::CertificateDer::from_pem_slice(ROTATED_CERT).unwrap())
            .unwrap();
    }
    let config =
        ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_root_certificates(roots)
            .with_no_client_auth();
    let connection =
        ClientConnection::new(Arc::new(config), ServerName::try_from("localhost").unwrap())
            .unwrap();
    let mut stream = StreamOwned::new(connection, socket);
    while stream.conn.is_handshaking() {
        stream.conn.complete_io(&mut stream.sock)?;
    }
    Ok(stream)
}
struct WireReply {
    status: u16,
    headers: BTreeMap<String, String>,
    body: Vec<u8>,
}
impl WireReply {
    fn json(&self) -> serde_json::Value {
        serde_json::from_slice(&self.body).unwrap()
    }
    fn error(&self, status: u16, code: &str) {
        assert_eq!(self.status, status);
        assert_eq!(self.body, format!("{{\"error\":\"{code}\"}}").as_bytes());
    }
    fn hides(&self, secret: &str) {
        assert!(!String::from_utf8_lossy(&self.body).contains(secret));
        assert!(!self.headers.values().any(|s| s.contains(secret)));
    }
}
fn read_reply(stream: impl Read) -> std::io::Result<WireReply> {
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let status = line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| std::io::Error::other("no HTTP status"))?;
    let mut headers = BTreeMap::new();
    loop {
        line.clear();
        reader.read_line(&mut line)?;
        if line == "\r\n" {
            break;
        }
        if line.is_empty() {
            return Err(std::io::Error::other("short HTTP headers"));
        }
        let (name, value) = line
            .split_once(':')
            .ok_or_else(|| std::io::Error::other("invalid HTTP header"))?;
        headers.insert(name.to_ascii_lowercase(), value.trim().to_owned());
    }
    assert!(
        !headers
            .keys()
            .any(|name| name.starts_with("access-control-")),
        "CORS must be absent"
    );
    let size: usize = headers.get("content-length").unwrap().parse().unwrap();
    assert!(size <= 1024 * 1024);
    let mut body = vec![0; size];
    reader.read_exact(&mut body)?;
    Ok(WireReply {
        status,
        headers,
        body,
    })
}

#[test]
fn tls_only_and_explicit_fixture_trust() {
    let f = Fixture::new("minecraft", 8, 1024, 3);
    f.request("GET", "/api/v2/pack/status?installed=0", b"", None)
        .error_if_unpublished();
    assert!(connect(f.address, false).is_err());
    let mut socket = TcpStream::connect(f.address).unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(4)))
        .unwrap();
    socket
        .write_all(b"GET /api/v2/pack/status?installed=0 HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .unwrap();
    let mut bytes = [0; 128];
    if let Ok(n) = socket.read(&mut bytes) {
        assert!(!bytes[..n].starts_with(b"HTTP/"));
    }
}
impl WireReply {
    fn error_if_unpublished(&self) {
        assert_eq!(self.status, 200);
        assert_eq!(self.json(), serde_json::json!({"published":false}));
    }
}

#[test]
fn discovery_negotiates_and_rejects_unsupported_versions() {
    let f = Fixture::new("minecraft", 8, 1024, 3);
    let request = discovery::encode_request(&DiscoveryRequest::new(vec![1, 2]).unwrap()).unwrap();
    let reply = f.request("POST", "/api/v2/discovery", &request, None);
    assert_eq!(reply.status, 200);
    let manifest = discovery::decode_manifest(&reply.body).unwrap();
    assert_eq!(manifest.server_id, f.host().database().server_id());
    let unsupported = discovery::encode_request(&DiscoveryRequest::new(vec![99]).unwrap()).unwrap();
    let reply = f.request("POST", "/api/v2/discovery", &unsupported, None);
    assert_eq!(reply.status, 400);
    assert_eq!(
        discovery::decode_failure(&reply.body).unwrap().error,
        discovery::ErrorCode::UnsupportedProtocol
    );
}

#[test]
fn status_tracks_published_revisions_without_sensitive_configuration() {
    let f = Fixture::new("minecraft", 8, 1024, 3);
    f.request("GET", "/api/v2/pack/status?installed=0", b"", None)
        .error_if_unpublished();
    let owner = f.owner();
    f.publish(
        &owner,
        UpdatePolicy::Optional,
        b"first fixture archive",
        "1.0",
    );
    assert_eq!(
        f.request("GET", "/api/v2/pack/status?installed=1", b"", None)
            .json()["status"],
        "current"
    );
    f.publish(
        &owner,
        UpdatePolicy::Optional,
        b"second fixture archive",
        "1.1",
    );
    let reply = f.request("GET", "/api/v2/pack/status?installed=1", b"", None);
    assert_eq!(reply.json()["status"], "update_available");
    assert_eq!(reply.json()["revision"], 2);
    assert_eq!(reply.json()["minimum_revision"], 1);
    assert_eq!(reply.json()["name"], "Hosted");
    assert_eq!(reply.json()["version"], "1.0.0");
    assert_eq!(reply.json()["display_version"], "1.1");
    f.publish(
        &owner,
        UpdatePolicy::Required,
        b"third fixture archive",
        "2.0",
    );
    let reply = f.request("GET", "/api/v2/pack/status?installed=1", b"", None);
    assert_eq!(reply.json()["status"], "update_required");
    assert_eq!(reply.json().as_object().unwrap().len(), 7);
}

#[test]
fn password_session_artifact_ticket_and_revocation_are_pack_only() {
    let f = Fixture::new("minecraft-or-password", 8, 4096, 10);
    let owner = f.owner();
    let passwords = f.host().pack_passwords().unwrap();
    let metadata = passwords
        .set(
            &owner,
            PasswordRevision::new(0).unwrap(),
            &SecretPassword::try_from(PASSWORD.as_bytes().to_vec()).unwrap(),
        )
        .unwrap();
    f.request(
        "POST",
        "/api/v2/auth/password",
        b"{\"password\":\"wrong password\"}",
        None,
    )
    .error(403, "pack_access_denied");
    let reply = f.request(
        "POST",
        "/api/v2/auth/password",
        &serde_json::to_vec(&serde_json::json!({"password":PASSWORD})).unwrap(),
        None,
    );
    assert_eq!(reply.status, 200);
    let session = reply.json()["session"].as_str().unwrap().to_owned();
    assert_eq!(session.len(), 43);
    assert_eq!(reply.json()["expires_in"], 3600);
    reply.hides(PASSWORD);
    f.request(
        "GET",
        "/api/v2/pack/status?installed=0",
        b"",
        Some(&session),
    )
    .hides(&session);
    f.request("GET", "/unknown", b"", Some(&session))
        .hides(&session);
    let discovery_body =
        discovery::encode_request(&DiscoveryRequest::new(vec![1]).unwrap()).unwrap();
    f.request("POST", "/api/v2/discovery", &discovery_body, Some(&session))
        .hides(&session);
    f.request("GET", "/api/v2/pack/artifact", b"", Some(&session))
        .error(404, "pack_not_published");
    f.request("GET", "/api/v2/pack/artifact", b"", None)
        .error(401, "pack_session_invalid");
    f.request("GET", "/api/v2/pack/artifact", b"", Some(&"a".repeat(43)))
        .error(401, "pack_session_invalid");
    f.request("POST", "/api/v2/pack/ticket", b"", None)
        .error(401, "pack_session_invalid");
    let first = f.request("POST", "/api/v2/pack/ticket", b"", Some(&session));
    assert_eq!(first.status, 200);
    first.hides(&session);
    let second = f.request("POST", "/api/v2/pack/ticket", b"", Some(&session));
    assert_ne!(first.json()["ticket"], second.json()["ticket"]);
    second.hides(&session);
    let ticket = first.json()["ticket"].as_str().unwrap().to_owned();
    assert_eq!(ticket.len(), 43);
    let policy = f.host().pack_policy().unwrap();
    f.host()
        .pack_sessions()
        .redeem_ticket(&ticket, Instant::now(), &policy)
        .unwrap();
    assert!(
        f.host()
            .pack_sessions()
            .redeem_ticket(&ticket, Instant::now(), &policy)
            .is_err()
    );
    let payload = b"exact hosted fixture archive bytes";
    let release = f.publish(&owner, UpdatePolicy::Optional, payload, "1.0");
    let reply = f.request("GET", "/api/v2/pack/artifact", b"", Some(&session));
    assert_eq!(reply.status, 200);
    assert_eq!(reply.body, payload);
    reply.hides(&session);
    assert_eq!(reply.headers["content-length"], payload.len().to_string());
    assert_eq!(reply.headers["content-type"], "application/zip");
    assert_eq!(
        reply.headers["etag"],
        format!("\"{}\"", release.artifact().sha256().to_hex())
    );
    for path in [
        "/api/v2/manage/power",
        "/api/v2/manage/security",
        "/api/v2/manage/fs",
    ] {
        let r = f.request("POST", path, b"", Some(&session));
        r.error(403, "management_unavailable");
        r.hides(&session);
    }
    for path in [
        "/api/v1/server/power",
        "/api/v1/admin/security",
        "/api/v1/console",
        "/api/v1/fs/read",
        "/api/v1/auth/token",
    ] {
        let r = f.request("POST", path, b"", Some(&session));
        r.error(410, "legacy_endpoint_removed");
        r.hides(&session);
    }
    passwords.revoke(&owner, metadata.revision()).unwrap();
    let reply = f.request("GET", "/api/v2/pack/artifact", b"", Some(&session));
    reply.error(401, "pack_session_invalid");
    reply.hides(&session);
    f.request("POST", "/api/v2/pack/ticket", b"", Some(&session))
        .error(401, "pack_session_invalid");
    assert_eq!(f.host().runtime().game_snapshot().unwrap().generation, None);
}

#[test]
fn legacy_management_mutations_are_refused_on_loopback_without_credentials_or_effects() {
    let f = Fixture::new("minecraft", 8, 1024, 3);
    for method in ["GET", "POST", "PUT", "DELETE"] {
        for path in [
            "/api/v1/server/power",
            "/api/v1/console",
            "/api/v1/admin/security",
            "/api/v1/private/files",
            "/api/v1/files/upload",
            "/api/v1/content/remove",
            "/api/v1/fs/read",
        ] {
            f.request(method, path, b"{\"action\":\"start\"}", None)
                .error(410, "legacy_endpoint_removed");
        }
        for path in [
            "/api/v2/manage/power",
            "/api/v2/manage/console",
            "/api/v2/manage/security",
            "/api/v2/manage/fs",
        ] {
            f.request(method, path, b"{\"action\":\"start\"}", None)
                .error(403, "management_unavailable");
        }
    }
    assert_eq!(f.host().runtime().game_snapshot().unwrap().generation, None);
    let principals: i64 = f
        .host()
        .database()
        .read(|connection| {
            Ok(connection.query_row("SELECT COUNT(*) FROM principals", [], |row| row.get(0))?)
        })
        .unwrap();
    assert_eq!(principals, 0);
    assert!(!f.temp.path().join("game/server.properties").exists());
}

#[test]
fn password_rotation_applies_to_http_sessions_and_preserves_pack_publication() {
    let f = Fixture::new("minecraft-or-password", 8, 4096, 10);
    let owner = f.owner();
    let session = f.password_session(&owner);
    let payload = b"unchanged publication after pack password rotation";
    let release = f.publish(&owner, UpdatePolicy::Optional, payload, "1.0");
    let pending = f.request("POST", "/api/v2/pack/ticket", b"", Some(&session));
    assert_eq!(pending.status, 200);
    let ticket = pending.json()["ticket"].as_str().unwrap().to_owned();
    let rotated_password = "replacement transport fixture password";
    f.host()
        .pack_passwords()
        .unwrap()
        .set(
            &owner,
            PasswordRevision::new(1).unwrap(),
            &SecretPassword::try_from(rotated_password.as_bytes().to_vec()).unwrap(),
        )
        .unwrap();
    f.request("GET", "/api/v2/pack/artifact", b"", Some(&session))
        .error(401, "pack_session_invalid");
    f.request("POST", "/api/v2/realtime/ticket", b"", Some(&session))
        .error(401, "pack_session_invalid");
    assert!(
        f.host()
            .pack_sessions()
            .redeem_ticket(&ticket, Instant::now(), &f.host().pack_policy().unwrap())
            .is_err()
    );
    f.request(
        "POST",
        "/api/v2/auth/password",
        &serde_json::to_vec(&serde_json::json!({"password":PASSWORD})).unwrap(),
        None,
    )
    .error(403, "pack_access_denied");
    let fresh = f.request(
        "POST",
        "/api/v2/auth/password",
        &serde_json::to_vec(&serde_json::json!({"password":rotated_password})).unwrap(),
        None,
    );
    assert_eq!(fresh.status, 200);
    let fresh_session = fresh.json()["session"].as_str().unwrap().to_owned();
    assert_ne!(fresh_session, session);
    let artifact = f.request("GET", "/api/v2/pack/artifact", b"", Some(&fresh_session));
    assert_eq!(artifact.status, 200);
    assert_eq!(artifact.body, payload);
    assert_eq!(f.host().releases().current().unwrap(), Some(release));
    assert_eq!(f.host().runtime().game_snapshot().unwrap().generation, None);
}

#[test]
fn minecraft_mode_password_refusal_and_unknown_completion_never_contact_provider() {
    let f = Fixture::new("minecraft", 8, 1024, 3);
    f.request(
        "POST",
        "/api/v2/auth/password",
        b"{\"password\":\"fixture password\"}",
        None,
    )
    .error(403, "pack_access_denied");
    let reply = f.request(
        "POST",
        "/api/v2/auth/minecraft/challenge",
        b"{\"uuid\":\"12345678-1234-4234-8234-123456789abc\",\"name\":\"FixturePlayer\"}",
        None,
    );
    assert_eq!(reply.status, 200);
    assert_eq!(reply.json()["challenge"].as_str().unwrap().len(), 64);
    assert!(reply.json()["server_hash"].is_string());
    assert_eq!(reply.json()["expires_in"], 60);
    // This ID is unknown, not the issued challenge: removal returns before provider I/O.
    let input = format!("{{\"challenge\":\"{}\"}}", "0".repeat(64));
    f.request(
        "POST",
        "/api/v2/auth/minecraft/complete",
        input.as_bytes(),
        None,
    )
    .error(403, "pack_access_denied");
}

#[test]
fn strict_bodies_unknown_routes_and_oversized_declaration_refuse_without_body_read() {
    let f = Fixture::new("minecraft", 8, 256, 3);
    for bytes in [
        b"{".as_slice(),
        b"{\"password\":\"fixture\",\"admin\":true}",
        b"{\"password\":\"one\",\"password\":\"two\"}",
    ] {
        f.request("POST", "/api/v2/auth/password", bytes, None)
            .error(400, "invalid_request");
    }
    f.request("GET", "/unknown", b"", None)
        .error(404, "endpoint_not_found");
    f.request("GET", "/api/v2/pack/status?installed=1&extra=1", b"", None)
        .error(400, "invalid_request");
    let mut stream = f.stream();
    stream.write_all(b"POST /api/v2/auth/password HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: 1000000\r\n\r\n").unwrap();
    stream.flush().unwrap();
    let started = Instant::now();
    read_reply(stream).unwrap().error(413, "request_too_large");
    assert!(started.elapsed() < Duration::from_secs(2));
}

#[test]
fn peer_rate_limit_uses_socket_not_forwarding_headers() {
    let f = Fixture::new("minecraft", 8, 1024, 5);
    let mut limited = false;
    for index in 0..80 {
        let mut stream = f.stream();
        write!(stream, "GET /api/v2/pack/status?installed=0 HTTP/1.1\r\nHost: localhost\r\nX-Forwarded-For: 192.0.2.{index}\r\nConnection: close\r\n\r\n").unwrap();
        stream.flush().unwrap();
        let reply = read_reply(stream).unwrap();
        if reply.status == 429 {
            reply.error(429, "peer_rate_limited");
            assert!(reply.headers["retry-after"].parse::<u64>().unwrap() >= 1);
            limited = true;
            break;
        }
        assert_eq!(reply.status, 200);
    }
    assert!(limited);
    // Pack rate exhaustion cannot turn fixed legacy/management denials into
    // compatibility successes (or into ambiguous authentication responses).
    f.request("POST", "/api/v2/manage/power", b"", None)
        .error(403, "management_unavailable");
    f.request("POST", "/api/v1/auth/token", b"", None)
        .error(410, "legacy_endpoint_removed");
}

#[test]
fn connection_cap_and_slow_client_timeout_fail_closed() {
    let f = Fixture::new("minecraft", 1, 1024, 1);
    let mut first = f.stream();
    first.write_all(b"GET /api/v2/").unwrap();
    first.flush().unwrap();
    assert!(
        connect(f.address, true).is_err(),
        "no second connection admitted"
    );
    let started = Instant::now();
    let mut byte = [0; 1];
    assert!(!matches!(first.read(&mut byte), Ok(1)));
    assert!(started.elapsed() < Duration::from_secs(3));
    // The slot is released after the timeout; fresh peers can handshake again.
    f.request("GET", "/api/v2/pack/status?installed=0", b"", None)
        .error_if_unpublished();
}

#[test]
fn shutdown_joins_idle_keepalive_and_database_can_reopen() {
    let mut f = Fixture::new("minecraft", 8, 1024, 30);
    let mut idle = f.stream();
    idle.write_all(b"GET /api/v2/pack/status?installed=0 HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .unwrap();
    idle.flush().unwrap();
    read_reply(&mut idle).unwrap().error_if_unpublished();
    // Leave a successfully handshaken, HTTP keepalive peer open throughout shutdown.
    let started = Instant::now();
    let transport = f.transport.take().unwrap();
    assert!(matches!(
        transport.shutdown(Duration::from_secs(2)).unwrap(),
        TransportShutdown::Complete
    ));
    assert!(started.elapsed() < Duration::from_secs(2));
    assert!(TcpStream::connect(f.address).is_err());
    drop(idle);
    f.finish();
    let directory = StateDirectory::new(
        f.temp.path().join("private-state"),
        Some(&f.temp.path().join("game")),
    )
    .unwrap();
    let reopened = StateDatabase::open(directory, Arc::new(SystemClock)).unwrap();
    reopened.close().unwrap();
}

#[test]
fn missing_or_invalid_tls_material_never_generates_trust_or_binds() {
    let f = Fixture::new("minecraft", 8, 1024, 3);
    let ListenerDescription::Tls(listener) = f.host().configuration().listener() else {
        panic!("TLS fixture");
    };
    let listener = listener.on_ephemeral_port();
    std::fs::remove_file(f.temp.path().join("private/key.pem")).unwrap();
    assert_eq!(
        Transport::start(f.host().network_handle(), &listener)
            .unwrap_err()
            .code(),
        "tls_material_unavailable"
    );
    assert!(!f.temp.path().join("private/key.pem").exists());
    std::fs::write(
        f.temp.path().join("private/key.pem"),
        b"not a PEM private key",
    )
    .unwrap();
    assert_eq!(
        Transport::start(f.host().network_handle(), &listener)
            .unwrap_err()
            .code(),
        "tls_material_unavailable"
    );
}

#[test]
fn chunked_body_is_stopped_at_limit_without_waiting_for_terminator() {
    let f = Fixture::new("minecraft", 8, 256, 3);
    let mut stream = f.stream();
    stream.write_all(b"POST /api/v2/auth/password HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nTransfer-Encoding: chunked\r\n\r\n12c\r\n").unwrap();
    stream.write_all(&[b'x'; 300]).unwrap();
    stream.write_all(b"\r\n").unwrap();
    stream.flush().unwrap();
    // No zero-sized chunk is sent, so a server that collects the entire body hangs.
    let started = Instant::now();
    read_reply(stream).unwrap().error(413, "request_too_large");
    assert!(started.elapsed() < Duration::from_secs(2));
}

#[test]
fn header_count_and_header_bytes_are_bounded() {
    let f = Fixture::new("minecraft", 8, 1024, 3);
    for request in [
        format!(
            "GET /api/v2/pack/status?installed=0 HTTP/1.1\r\nHost: localhost\r\nX-Large: {}\r\n\r\n",
            "x".repeat(20 * 1024)
        ),
        format!(
            "GET /api/v2/pack/status?installed=0 HTTP/1.1\r\nHost: localhost\r\n{}\r\n",
            (0..80)
                .map(|i| format!("X-{i}: fixture\r\n"))
                .collect::<String>()
        ),
    ] {
        let mut stream = f.stream();
        let _ = stream.write_all(request.as_bytes());
        let _ = stream.flush();
        // Hyper itself rejects malformed/oversized heads before route dispatch.
        let mut bytes = [0; 512];
        match stream.read(&mut bytes) {
            Ok(0) | Err(_) => {}
            Ok(n) => {
                let response = String::from_utf8_lossy(&bytes[..n]);
                assert!(
                    response.starts_with("HTTP/1.1 431") || response.starts_with("HTTP/1.1 400")
                );
                assert!(!response.to_ascii_lowercase().contains("access-control-"));
                assert!(!response.contains("xxxx"));
            }
        }
    }
}

#[test]
fn incomplete_request_body_hits_the_configured_timeout() {
    let f = Fixture::new("minecraft", 8, 1024, 1);
    let mut stream = f.stream();
    stream.write_all(b"POST /api/v2/auth/password HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: 100\r\n\r\n{").unwrap();
    stream.flush().unwrap();
    let started = Instant::now();
    let mut bytes = [0; 128];
    match stream.read(&mut bytes) {
        Ok(0) | Err(_) => {}
        Ok(n) => assert!(String::from_utf8_lossy(&bytes[..n]).starts_with("HTTP/1.1 408")),
    }
    assert!(started.elapsed() < Duration::from_secs(3));
}

impl Fixture {
    fn password_session(&self, owner: &Subject) -> String {
        self.host()
            .pack_passwords()
            .unwrap()
            .set(
                owner,
                PasswordRevision::new(0).unwrap(),
                &SecretPassword::try_from(PASSWORD.as_bytes().to_vec()).unwrap(),
            )
            .unwrap();
        self.request(
            "POST",
            "/api/v2/auth/password",
            &serde_json::to_vec(&serde_json::json!({"password":PASSWORD})).unwrap(),
            None,
        )
        .json()["session"]
            .as_str()
            .unwrap()
            .to_owned()
    }
    fn raw(&self, request: &[u8]) -> WireReply {
        let mut stream = self.stream();
        stream.write_all(request).unwrap();
        stream.flush().unwrap();
        read_reply(stream).unwrap()
    }
}

#[test]
fn slow_large_downloads_are_globally_bounded_and_cancel_releases_budget() {
    let f = Fixture::new("minecraft-or-password", 8, 4096, 30);
    let owner = f.owner();
    let session = f.password_session(&owner);
    f.publish(
        &owner,
        UpdatePolicy::Optional,
        &vec![b'x'; 16 * 1024 * 1024],
        "large",
    );
    let mut receivers = Vec::new();
    for _ in 0..2 {
        let mut stream = f.stream();
        write!(stream, "GET /api/v2/pack/artifact HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {session}\r\n\r\n").unwrap();
        stream.flush().unwrap();
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        assert!(line.starts_with("HTTP/1.1 200"), "{line}");
        loop {
            line.clear();
            reader.read_line(&mut line).unwrap();
            if line == "\r\n" {
                break;
            }
            assert!(!line.is_empty());
        }
        receivers.push(reader);
    }
    f.request("GET", "/api/v2/pack/artifact", b"", Some(&session))
        .error(503, "download_busy");
    drop(receivers.pop());
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let mut stream = f.stream();
        write!(stream, "GET /api/v2/pack/artifact HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {session}\r\nConnection: close\r\n\r\n").unwrap();
        stream.flush().unwrap();
        let mut reader = BufReader::new(stream);
        let mut status = String::new();
        reader.read_line(&mut status).unwrap();
        if status.starts_with("HTTP/1.1 200") {
            break;
        }
        assert!(status.starts_with("HTTP/1.1 503"), "{status}");
        assert!(
            Instant::now() < deadline,
            "cancelled download retained its budget"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[test]
fn bearer_domains_duplicate_headers_and_cookie_query_credentials_are_refused() {
    let f = Fixture::new("minecraft-or-password", 8, 4096, 10);
    let owner = f.owner();
    let session = f.password_session(&owner);
    // Download tickets are now usable for artifacts; realtime tickets must
    // remain outside both the session and download credential domains.
    let ticket = f
        .request("POST", "/api/v2/realtime/ticket", b"", Some(&session))
        .json()["ticket"]
        .as_str()
        .unwrap()
        .to_owned();
    f.request("POST", "/api/v2/pack/ticket", b"", Some(&ticket))
        .error(401, "pack_session_invalid");
    f.request("GET", "/api/v2/pack/artifact", b"", Some(&ticket))
        .error(401, "pack_session_invalid");
    let network = f.host().network_handle();
    assert!(
        network
            .with_policy(|p| network
                .sessions()
                .redeem_ticket(&session, Instant::now(), p))
            .unwrap()
            .is_err()
    );
    f.raw(format!("GET /api/v2/pack/artifact HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {session}\r\nAuthorization: Bearer {session}\r\n\r\n").as_bytes()).error(401, "pack_session_invalid");
    f.request(
        "GET",
        &format!("/api/v2/pack/artifact?session={session}"),
        b"",
        None,
    )
    .error(400, "invalid_request");
    f.raw(format!("GET /api/v2/pack/artifact HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {session}\r\nCookie: session={session}\r\n\r\n").as_bytes()).error(400, "invalid_request");
}

#[test]
fn missing_content_length_post_is_empty_and_refused_body_cannot_pipeline() {
    let f = Fixture::new("minecraft", 8, 256, 3);
    f.raw(b"POST /api/v2/auth/password HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\n\r\n").error(400, "invalid_request");
    let mut stream = f.stream();
    let body = "x".repeat(300);
    write!(stream, "POST /api/v2/auth/password HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: 300\r\n\r\n{body}GET /api/v2/pack/status?installed=0 HTTP/1.1\r\nHost: localhost\r\n\r\n").unwrap();
    stream.flush().unwrap();
    let mut reader = BufReader::new(stream);
    read_reply(&mut reader)
        .unwrap()
        .error(413, "request_too_large");
    let mut rest = Vec::new();
    let _ = reader.read_to_end(&mut rest);
    assert!(!String::from_utf8_lossy(&rest).contains("HTTP/1.1 200"));
}

#[test]
fn tls_only_churn_exhausts_pre_handshake_peer_budget() {
    let f = Fixture::new("minecraft", 8, 1024, 3);
    let mut successes = 0;
    for _ in 0..140 {
        match connect(f.address, true) {
            Ok(stream) => {
                successes += 1;
                drop(stream);
            }
            Err(_) => break,
        }
    }
    // 120 per minute refills continuously (2/s), so a few seconds of churn may earn a little more.
    assert!(successes > 0 && successes <= 126, "successes={successes}");
    assert!(
        connect(f.address, true).is_err(),
        "headers are not required to consume peer budget"
    );
}

#[test]
fn tls_below_12_is_rejected_and_mismatched_key_fails_start() {
    let f = Fixture::new("minecraft", 8, 1024, 3);
    // A minimal TLS 1.1 ClientHello, intentionally offering no TLS 1.2/1.3.
    let mut hello = vec![3, 2];
    hello.extend_from_slice(&[0; 32]);
    hello.extend_from_slice(&[0, 0, 2, 0, 0x2f, 1, 0]);
    let mut handshake = vec![1, 0, 0, hello.len() as u8];
    handshake.extend(hello);
    let mut record = vec![22, 3, 2, 0, handshake.len() as u8];
    record.extend(handshake);
    let mut socket = TcpStream::connect(f.address).unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    socket.write_all(&record).unwrap();
    let mut alert = [0; 7];
    socket.read_exact(&mut alert).unwrap();
    assert_eq!(alert[0], 21, "must send a TLS alert, not HTTP");
    // rustls answers a TLS 1.1-only hello with protocol_version or handshake_failure.
    assert_eq!(alert[5], 2, "fatal alert");
    assert!(matches!(alert[6], 40 | 70), "alert {}", alert[6]);
    let ListenerDescription::Tls(listener) = f.host().configuration().listener() else {
        panic!("TLS fixture")
    };
    std::fs::write(
        f.temp.path().join("private/key.pem"),
        include_bytes!("fixtures/test-only-mismatched-key.pem"),
    )
    .unwrap();
    assert_eq!(
        Transport::start(f.host().network_handle(), &listener.on_ephemeral_port())
            .unwrap_err()
            .code(),
        "tls_material_unavailable"
    );
}

#[test]
fn disk_stream_large_artifact_matches_published_identity_over_https() {
    let f = Fixture::new("minecraft-or-password", 8, 4096, 30);
    let owner = f.owner();
    let session = f.password_session(&owner);
    let payload: Vec<u8> = (0..11 * 64 * 1024 + 37).map(|n| (n % 251) as u8).collect();
    let release = f.publish(&owner, UpdatePolicy::Optional, &payload, "streamed");
    let reply = f.request("GET", "/api/v2/pack/artifact", b"", Some(&session));
    assert_eq!(reply.status, 200);
    assert_eq!(reply.body, payload);
    assert_eq!(reply.headers["content-length"], payload.len().to_string());
    assert_eq!(
        reply.headers["etag"],
        format!("\"{}\"", release.artifact().sha256().to_hex())
    );
}

#[test]
fn replaced_artifact_aborts_https_and_wrong_length_is_refused_before_headers() {
    let f = Fixture::new("minecraft-or-password", 8, 4096, 30);
    let owner = f.owner();
    let session = f.password_session(&owner);
    let payload = vec![b'x'; 9 * 64 * 1024];
    let release = f.publish(&owner, UpdatePolicy::Optional, &payload, "streamed");
    let path = f
        .temp
        .path()
        .join("private-state/artifacts")
        .join(format!("{}.zip", release.artifact().sha256().to_hex()));
    // Replace the pinned spelling with a same-length, different regular file.
    std::fs::remove_file(&path).unwrap();
    std::fs::write(&path, vec![b'y'; payload.len()]).unwrap();
    let mut stream = f.stream();
    write!(stream, "GET /api/v2/pack/artifact HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {session}\r\nConnection: close\r\n\r\n").unwrap();
    stream.flush().unwrap();
    assert!(
        read_reply(stream).is_err(),
        "mismatched response must never complete its advertised length"
    );
    // Size must be checked before advertising the published Content-Length.
    std::fs::write(&path, b"truncated").unwrap();
    f.request("GET", "/api/v2/pack/artifact", b"", Some(&session))
        .error(503, "pack_unavailable");
}

#[path = "support/realtime.rs"]
mod realtime;

fn fingerprint(pem: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let der = rustls_pki_types::CertificateDer::from_pem_slice(pem).unwrap();
    hex::encode(Sha256::digest(der.as_ref()))
}
fn peer_fingerprint(stream: &TlsStream) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(
        stream.conn.peer_certificates().unwrap()[0].as_ref(),
    ))
}
fn discovery_on(stream: &mut TlsStream) -> WireReply {
    let request = discovery::encode_request(&DiscoveryRequest::new(vec![1]).unwrap()).unwrap();
    write!(
        stream,
        "POST /api/v2/discovery HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
        request.len()
    )
    .unwrap();
    stream.write_all(&request).unwrap();
    stream.flush().unwrap();
    let reply = read_reply(stream).unwrap();
    assert_eq!(
        reply.status,
        200,
        "{}",
        String::from_utf8_lossy(&reply.body)
    );
    reply
}

#[test]
fn rotation_changes_new_handshakes_and_discovery_but_preserves_open_stream() {
    let f = Fixture::new("minecraft", 8, 1024, 30);
    let transport = f.transport.as_ref().unwrap();
    assert!(!transport.reload_tls().unwrap());
    let old = fingerprint(CERT);
    let new = fingerprint(ROTATED_CERT);
    assert_ne!(old, new);
    let mut existing = f.stream();
    assert_eq!(peer_fingerprint(&existing), old);
    assert_eq!(
        discovery_on(&mut existing).headers["x-vesta-tls-sha256-fingerprint"],
        old
    );
    std::fs::write(f.temp.path().join("private/cert.pem"), ROTATED_CERT).unwrap();
    // A half-installed pair is rejected and remains retryable.
    assert_eq!(
        transport.reload_tls().unwrap_err().code(),
        "tls_material_unavailable"
    );
    assert_eq!(transport.tls_fingerprint().unwrap(), old);
    std::fs::write(f.temp.path().join("private/key.pem"), ROTATED_KEY).unwrap();
    assert!(transport.reload_tls().unwrap());
    assert!(transport.last_tls_reload_error().is_none());
    assert_eq!(transport.tls_fingerprint().unwrap(), new);
    assert!(!transport.reload_tls().unwrap());
    assert_eq!(peer_fingerprint(&f.stream()), new);
    let reply = discovery_on(&mut existing);
    assert_eq!(reply.status, 200);
    discovery::decode_manifest(&reply.body).unwrap();
    assert_eq!(reply.headers["x-vesta-tls-sha256-fingerprint"], new);
    assert_eq!(peer_fingerprint(&existing), old);
    let mut fresh = f.stream();
    assert_eq!(
        discovery_on(&mut fresh).headers["x-vesta-tls-sha256-fingerprint"],
        new
    );
}

#[test]
fn invalid_replacements_retain_last_good_certificate_and_redacted_error() {
    let f = Fixture::new("minecraft", 8, 1024, 30);
    let transport = f.transport.as_ref().unwrap();
    let old = fingerprint(CERT);
    let partial_chain = [
        CERT,
        b"-----BEGIN CERTIFICATE-----
YWJj
",
    ]
    .concat();
    let oversized = vec![b'x'; 256 * 1024 + 1];
    let cases: &[(&[u8], &[u8])] = &[
        (CERT, b"not a private key"),
        (CERT, ROTATED_KEY),
        (&CERT[..CERT.len() / 2], KEY),
        (CERT, &KEY[..KEY.len() / 2]),
        (&partial_chain, KEY),
        (&oversized, KEY),
        (
            b"-----BEGIN CERTIFICATE-----
YWJj
-----END CERTIFICATE-----
",
            KEY,
        ),
    ];
    for (cert, key) in cases {
        std::fs::write(f.temp.path().join("private/cert.pem"), cert).unwrap();
        std::fs::write(f.temp.path().join("private/key.pem"), key).unwrap();
        let error = transport.reload_tls().unwrap_err();
        assert_eq!(error.to_string(), "tls_material_unavailable");
        assert_eq!(transport.last_tls_reload_error(), Some(error));
        assert_eq!(transport.tls_fingerprint().unwrap(), old);
        let mut stream = f.stream();
        assert_eq!(peer_fingerprint(&stream), old);
        assert_eq!(
            discovery_on(&mut stream).headers["x-vesta-tls-sha256-fingerprint"],
            old
        );
    }
    std::fs::remove_file(f.temp.path().join("private/cert.pem")).unwrap();
    assert!(transport.reload_tls().is_err());
    assert_eq!(peer_fingerprint(&f.stream()), old);
    std::fs::write(f.temp.path().join("private/cert.pem"), CERT).unwrap();
    std::fs::write(f.temp.path().join("private/key.pem"), KEY).unwrap();
    transport.reload_tls().unwrap();
    assert!(transport.last_tls_reload_error().is_none());
}

#[test]
fn reload_hash_detects_change_even_with_unchanged_size_and_mtime() {
    let f = Fixture::new("minecraft", 8, 1024, 30);
    let transport = f.transport.as_ref().unwrap();
    let path = f.temp.path().join("private/key.pem");
    let modified = std::fs::metadata(&path).unwrap().modified().unwrap();
    let mut changed = KEY.to_vec();
    assert_eq!(changed.pop(), Some(b'\n'));
    changed.push(b' ');
    std::fs::write(&path, &changed).unwrap();
    std::fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_modified(modified))
        .unwrap();
    assert_eq!(
        std::fs::metadata(&path).unwrap().modified().unwrap(),
        modified
    );
    assert_eq!(std::fs::metadata(&path).unwrap().len(), KEY.len() as u64);
    assert!(transport.reload_tls().unwrap());
    assert!(!transport.reload_tls().unwrap());
    assert_eq!(peer_fingerprint(&f.stream()), fingerprint(CERT));
}
