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

#[derive(Default)]
struct FixtureClock(std::sync::atomic::AtomicI64);
impl vesta_state::Clock for FixtureClock {
    fn millis(&self) -> i64 {
        vesta_state::Clock::millis(&SystemClock) + self.0.load(std::sync::atomic::Ordering::SeqCst)
    }
}
struct Fixture {
    temp: TempDir,
    host: Option<Host>,
    transport: Option<Transport>,
    address: SocketAddr,
    clock: Arc<FixtureClock>,
    password_observation: std::sync::Mutex<Option<vesta_control::identity::PasswordVerification>>,
}
impl Fixture {
    fn new(mode: &str, connections: u16, max_body: u32, timeout: u16) -> Self {
        Self::configured(mode, connections, max_body, timeout, false)
    }
    fn inert() -> Self {
        Self::configured("minecraft", 8, 4096, 5, true)
    }
    fn configured(mode: &str, connections: u16, max_body: u32, timeout: u16, inert: bool) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let base = temp.path();
        for dir in ["game/config", "bin", "private"] {
            std::fs::create_dir_all(base.join(dir)).unwrap();
        }
        std::fs::write(base.join("bin/inert-placeholder"), b"").unwrap();
        std::fs::write(base.join("private/launch.toml"), "[minecraft]\nworking-directory = 'game'\n[launch]\nkind = 'native'\nexecutable = '../bin/inert-placeholder'\nstop = 'console-stop'\n").unwrap();
        if inert {
            std::fs::copy(
                std::env::current_exe().unwrap(),
                base.join("bin/owned-inert.exe"),
            )
            .unwrap();
            std::fs::write(
                base.join("game/tls-inert-enabled"),
                b"vesta-owned-tls-inert-v1",
            )
            .unwrap();
            std::fs::write(base.join("private/launch.toml"),"[minecraft]\nworking-directory = 'game'\n[launch]\nkind = 'native'\nexecutable = '../bin/owned-inert.exe'\nargs = ['--exact','management_inert_child','--nocapture','--test-threads=1']\nstop = 'console-stop'\n").unwrap();
        }
        std::fs::write(base.join("private/cert.pem"), CERT).unwrap();
        std::fs::write(base.join("private/key.pem"), KEY).unwrap();
        let document = format!(
            "schema-version = 1\n[paths]\nstate = 'private-state'\ngame = 'game'\nscan-roots = ['game/config']\nlaunch-definition = 'private/launch.toml'\n[pack]\nname = 'Hosted'\nversion = '1.0.0'\nrevision = 1\nminimum-revision = 1\nminecraft-version = '1.21.1'\nunresolved-disposition = 'embed-public'\n[pack-access]\nmode = '{mode}'\n[listener]\nmode = 'tls'\nbind = '127.0.0.1'\nport = 25567\ncertificate = 'private/cert.pem'\nprivate-key = 'private/key.pem'\nmax-connections = {connections}\nmax-request-bytes = {max_body}\nrequest-timeout-seconds = {timeout}\n"
        );
        let config =
            HostConfiguration::parse_toml(document.as_bytes(), base, Path::new("host.toml"))
                .unwrap();
        let ListenerDescription::Tls(listener) = config.listener() else {
            panic!("TLS fixture");
        };
        let listener = listener.on_ephemeral_port();
        let clock = Arc::new(FixtureClock::default());
        let host = Host::open(config, clock.clone()).unwrap();
        let transport = Transport::start(host.network_handle(), &listener).unwrap();
        let address = transport.local_addr();
        assert!(address.ip().is_loopback());
        assert_ne!(address.port(), 0);
        Self {
            temp,
            host: Some(host),
            transport: Some(transport),
            address,
            clock,
            password_observation: std::sync::Mutex::new(None),
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
        let _ = std::fs::write(self.temp.path().join("game/tls-inert-release"), b"release");
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
    read_buffered_reply(&mut BufReader::new(stream))
}
fn read_buffered_reply(reader: &mut impl BufRead) -> std::io::Result<WireReply> {
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
    // Text pages encode at most 4 MiB; disk ranges are capped at 4 MiB.
    assert!(size <= 6 * 1024 * 1024);
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
fn password_role_artifact_ticket_and_revocation_use_one_device_session() {
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
    let device_session = f.device_session();
    let reply = f.request(
        "POST",
        "/api/v2/auth/password",
        &serde_json::to_vec(&serde_json::json!({"password":PASSWORD})).unwrap(),
        Some(&device_session),
    );
    assert_eq!(reply.status, 200);
    let session = reply.json()["session"].as_str().unwrap().to_owned();
    assert_eq!(session.len(), 43);
    assert_eq!(reply.json()["expires_in"], 900);
    assert_eq!(session, device_session);
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
        Some(&session),
    );
    assert_eq!(fresh.status, 200);
    let fresh_session = fresh.json()["session"].as_str().unwrap().to_owned();
    assert_eq!(fresh_session, session);
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
fn successive_keepalive_requests_get_independent_head_and_response_budgets() {
    let f = Fixture::new("minecraft", 8, 1024, 1);
    let mut stream = f.stream();
    let started = Instant::now();
    for index in 0..3 {
        if index != 0 {
            // Near the end of the idle budget, begin a new request. Reading its
            // head takes a separate, bounded budget, not the old idle remainder.
            std::thread::sleep(Duration::from_millis(600));
        }
        stream
            .write_all(b"GET /api/v2/pack/status?installed=0 HTTP/1.1\r\n")
            .unwrap();
        stream.flush().unwrap();
        std::thread::sleep(Duration::from_millis(600));
        stream.write_all(b"Host: localhost\r\n\r\n").unwrap();
        stream.flush().unwrap();
        let reply = read_reply(&mut stream).unwrap();
        reply.error_if_unpublished();
        assert_ne!(
            reply.headers.get("connection").map(String::as_str),
            Some("close")
        );
    }
    assert!(started.elapsed() > Duration::from_secs(2));
}

#[test]
fn keepalive_idle_slot_expires_and_releases_connection_capacity() {
    let f = Fixture::new("minecraft", 1, 1024, 1);
    let mut stream = f.stream();
    stream
        .write_all(b"GET /api/v2/pack/status?installed=0 HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .unwrap();
    stream.flush().unwrap();
    read_reply(&mut stream).unwrap().error_if_unpublished();
    let started = Instant::now();
    let mut byte = [0; 1];
    assert!(!matches!(stream.read(&mut byte), Ok(1)));
    assert!(started.elapsed() < Duration::from_secs(2));
    f.request("GET", "/api/v2/pack/status?installed=0", b"", None)
        .error_if_unpublished();
}

#[test]
fn keepalive_trickled_partial_head_does_not_extend_request_deadline() {
    let f = Fixture::new("minecraft", 8, 1024, 1);
    let mut stream = f.stream();
    stream
        .write_all(b"GET /api/v2/pack/status?installed=0 HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .unwrap();
    stream.flush().unwrap();
    read_reply(&mut stream).unwrap().error_if_unpublished();
    std::thread::sleep(Duration::from_millis(500));
    let started = Instant::now();
    stream.write_all(b"GET /api/v2/").unwrap();
    stream.flush().unwrap();
    for _ in 0..4 {
        std::thread::sleep(Duration::from_millis(250));
        if stream
            .write_all(b"x")
            .and_then(|()| stream.flush())
            .is_err()
        {
            break;
        }
    }
    let mut byte = [0; 1];
    assert!(!matches!(stream.read(&mut byte), Ok(1)));
    assert!(
        started.elapsed() < Duration::from_millis(1650),
        "partial reads refreshed the head deadline"
    );
}

#[test]
fn keepalive_request_count_is_bounded_including_pipelined_heads() {
    let f = Fixture::new("minecraft", 8, 1024, 3);
    let mut stream = f.stream();
    // Preserve read-ahead across pipelined replies with a single BufReader.
    let request = b"GET /api/v2/pack/status?installed=0 HTTP/1.1\r\nHost: localhost\r\n\r\n";
    for _ in 0..33 {
        stream.write_all(request).unwrap();
    }
    stream.flush().unwrap();
    let mut reader = BufReader::new(stream);
    for index in 0..32 {
        let reply = read_buffered_reply(&mut reader).unwrap();
        reply.error_if_unpublished();
        assert_eq!(
            reply.headers.get("connection").map(String::as_str),
            (index == 31).then_some("close")
        );
    }
    let mut byte = [0; 1];
    assert!(
        !matches!(reader.read(&mut byte), Ok(1)),
        "request cap did not close the connection"
    );
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
        let token = self.device_session();
        let reply = self.request(
            "POST",
            "/api/v2/auth/password",
            &serde_json::to_vec(&serde_json::json!({"password":PASSWORD})).unwrap(),
            Some(&token),
        );
        assert_eq!(reply.status, 200);
        reply.json()["session"].as_str().unwrap().to_owned()
    }
    /// A live password-grant session without an HTTP KDF round trip, for fixtures
    /// whose request timeout is shorter than a debug-build password verification.
    fn direct_session(&self, owner: &Subject) -> String {
        self.host()
            .pack_passwords()
            .unwrap()
            .set(
                owner,
                PasswordRevision::new(0).unwrap(),
                &SecretPassword::try_from(PASSWORD.as_bytes().to_vec()).unwrap(),
            )
            .unwrap();
        self.granted_session()
    }
    fn device_session(&self) -> String {
        use base64::Engine as _;
        use ring::signature::KeyPair;
        let key = management_key();
        let management = self.host().management();
        let device = management
            .enroll_device(
                "TLS fixture",
                &base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(key.public_key().as_ref()),
            )
            .unwrap();
        let nonce = management.challenge(&device.device_id).unwrap();
        let signature = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(
            key.sign(&vesta_control::identity::management::session_message(
                management.server_id(),
                nonce.expose(),
            ))
            .as_ref(),
        );
        management
            .session(&device.device_id, nonce.expose(), &signature)
            .unwrap()
            .secret
            .expose()
            .to_owned()
    }
    fn granted_session(&self) -> String {
        let mut cache = self.password_observation.lock().unwrap();
        let verification = cache.get_or_insert_with(|| {
            self.host()
                .pack_passwords()
                .unwrap()
                .verify(PASSWORD.as_bytes().to_vec())
                .unwrap()
        });
        let token = self.device_session();
        self.host()
            .management()
            .grant_password(&token, verification, &self.host().pack_policy().unwrap())
            .unwrap();
        token
    }
    fn raw(&self, request: &[u8]) -> WireReply {
        let mut stream = self.stream();
        stream.write_all(request).unwrap();
        stream.flush().unwrap();
        read_reply(stream).unwrap()
    }
}

/// Send an artifact request and consume the head; returns the body reader.
fn artifact_head(f: &Fixture, credential: &str) -> (BufReader<TlsStream>, u16, usize) {
    let mut stream = f.stream();
    write!(stream, "GET /api/v2/pack/artifact HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {credential}\r\nConnection: close\r\n\r\n").unwrap();
    stream.flush().unwrap();
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    let status = line.split_whitespace().nth(1).unwrap().parse().unwrap();
    let mut length = 0;
    loop {
        line.clear();
        reader.read_line(&mut line).unwrap();
        if line == "\r\n" {
            break;
        }
        assert!(!line.is_empty());
        if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
            length = value.trim().parse().unwrap();
        }
    }
    (reader, status, length)
}

#[test]
fn slow_large_downloads_are_bounded_and_cancel_releases_budget() {
    let f = Fixture::new("minecraft-or-password", 8, 4096, 30);
    let owner = f.owner();
    let session = f.password_session(&owner);
    f.publish(
        &owner,
        UpdatePolicy::Optional,
        &vec![b'x'; 16 * 1024 * 1024],
        "large",
    );
    let (held, status, _) = artifact_head(&f, &session);
    assert_eq!(status, 200);
    // The same session (via bearer or its download ticket) cannot take a second slot.
    f.request("GET", "/api/v2/pack/artifact", b"", Some(&session))
        .error(503, "download_busy");
    let ticket = f
        .request("POST", "/api/v2/pack/ticket", b"", Some(&session))
        .json()["ticket"]
        .as_str()
        .unwrap()
        .to_owned();
    f.request("GET", "/api/v2/pack/artifact", b"", Some(&ticket))
        .error(503, "download_busy");
    // Another player's session from the same address is refused by the peer cap.
    let other = f.granted_session();
    f.request("GET", "/api/v2/pack/artifact", b"", Some(&other))
        .error(503, "download_busy");
    drop(held);
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
fn steady_download_outlives_request_timeout_and_completes() {
    // request_timeout is 1s; the throttled reader needs several seconds.
    let f = Fixture::new("minecraft-or-password", 8, 4096, 1);
    let owner = f.owner();
    let session = f.direct_session(&owner);
    let payload: Vec<u8> = (0..4 * 1024 * 1024).map(|n| (n % 253) as u8).collect();
    f.publish(&owner, UpdatePolicy::Optional, &payload, "slow-link");
    let started = Instant::now();
    let (mut reader, status, length) = artifact_head(&f, &session);
    assert_eq!((status, length), (200, payload.len()));
    let mut body = Vec::with_capacity(length);
    let mut chunk = vec![0; 64 * 1024];
    while body.len() < length {
        let count = reader.read(&mut chunk).unwrap();
        assert_ne!(count, 0, "download cut at {} of {length}", body.len());
        body.extend_from_slice(&chunk[..count]);
        // About 1 MiB/s: far above the floor, far below loopback speed.
        std::thread::sleep(Duration::from_millis(60));
    }
    assert!(
        started.elapsed() > Duration::from_secs(2),
        "fixture did not outlast request_timeout"
    );
    assert_eq!(body, payload);
}

#[test]
fn stalled_download_is_dropped_and_releases_its_session_slot() {
    let f = Fixture::new("minecraft-or-password", 8, 4096, 1);
    let owner = f.owner();
    let session = f.direct_session(&owner);
    // The largest pinned archive; the final read proves it never fully buffered.
    let payload = vec![b's'; 16 * 1024 * 1024];
    f.publish(&owner, UpdatePolicy::Optional, &payload, "stalled");
    let (mut stalled, status, length) = artifact_head(&f, &session);
    assert_eq!((status, length), (200, payload.len()));
    f.request("GET", "/api/v2/pack/artifact", b"", Some(&session))
        .error(503, "download_busy");
    // Without reading, the throughput floor must drop the connection and permits.
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let (mut reader, status, _) = artifact_head(&f, &session);
        if status == 200 {
            drop(reader);
            break;
        }
        let mut rest = Vec::new();
        let _ = reader.read_to_end(&mut rest);
        assert_eq!(status, 503);
        assert!(
            Instant::now() < deadline,
            "stalled download retained its session slot"
        );
        std::thread::sleep(Duration::from_millis(200));
    }
    stalled
        .get_mut()
        .sock
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut received = 0;
    let mut chunk = vec![0; 256 * 1024];
    loop {
        match stalled.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(count) => received += count,
        }
    }
    assert!(received < length, "stalled response must not complete");
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
    let local_fingerprint = transport.fingerprint_handle();
    let old = fingerprint(CERT);
    assert_eq!(local_fingerprint.current().unwrap(), old);
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
    assert_eq!(local_fingerprint.current().unwrap(), new);
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

fn management_key() -> ring::signature::Ed25519KeyPair {
    ring::signature::Ed25519KeyPair::from_pkcs8(
        ring::signature::Ed25519KeyPair::generate_pkcs8(&ring::rand::SystemRandom::new())
            .unwrap()
            .as_ref(),
    )
    .unwrap()
}
fn enroll_management(
    f: &Fixture,
    role: vesta_control::identity::BuiltinRole,
) -> (String, ring::signature::Ed25519KeyPair) {
    use base64::Engine as _;
    use ring::signature::KeyPair;
    let code = f.host().management().issue_local(role, 10).unwrap();
    let key = management_key();
    let request = serde_json::json!({"code": code.secret.expose(), "device_name": "TLS test device", "public_key": base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(key.public_key().as_ref())});
    let reply = f.request(
        "POST",
        "/api/v3/manage/pair",
        &serde_json::to_vec(&request).unwrap(),
        None,
    );
    assert_eq!(reply.status, 200);
    assert_eq!(reply.json()["role"], role.id());
    f.request(
        "POST",
        "/api/v3/manage/pair",
        &serde_json::to_vec(&request).unwrap(),
        None,
    )
    .error(401, "management_pair_invalid");
    (reply.json()["device_id"].as_str().unwrap().to_owned(), key)
}
fn management_session(f: &Fixture, device: &str, key: &ring::signature::Ed25519KeyPair) -> String {
    use base64::Engine as _;
    let request = serde_json::to_vec(&serde_json::json!({"device_id": device})).unwrap();
    let reply = f.request("POST", "/api/v3/manage/session/challenge", &request, None);
    assert_eq!(reply.status, 200);
    let data = reply.json();
    let message = vesta_control::identity::management::session_message(
        data["server_id"].as_str().unwrap(),
        data["nonce"].as_str().unwrap(),
    );
    let request = serde_json::to_vec(&serde_json::json!({"device_id": device, "nonce": data["nonce"], "signature": base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(key.sign(&message).as_ref())})).unwrap();
    let reply = f.request("POST", "/api/v3/manage/session", &request, None);
    assert_eq!(reply.status, 200);
    f.request("POST", "/api/v3/manage/session", &request, None)
        .error(401, "management_session_invalid");
    reply.json()["token"].as_str().unwrap().to_owned()
}
#[test]
fn management_https_pair_challenge_session_policy_revoke_and_no_remote_link_creation() {
    let f = Fixture::new("minecraft", 8, 1024, 3);
    let (device, key) = enroll_management(&f, vesta_control::identity::BuiltinRole::Owner);
    let token = management_session(&f, &device, &key);
    for path in [
        "/api/v3/manage",
        "/api/v3/manage/status",
        "/api/v3/manage/owner/link",
        "/api/v3/manage/unknown",
    ] {
        f.request("GET", path, b"", None)
            .error(401, "management_session_invalid");
        f.request("GET", path, b"", Some(&token))
            .error(501, "not_implemented");
    }
    f.request(
        "POST",
        "/api/v3/manage/owner/link",
        br#"{"minecraft_uuid":"11111111-2222-4333-8444-555555555555"}"#,
        Some(&token),
    )
    .error(501, "not_implemented");
    assert!(f.host().management().links_local().unwrap().is_empty());
    f.request("GET", "/api/v2/manage/status", b"", Some(&token))
        .error(403, "management_unavailable");
    f.request("GET", "/api/v1/status", b"", Some(&token))
        .error(410, "legacy_endpoint_removed");
    let db = f.host().database();
    db.write(|c| { c.execute("UPDATE role_assignments SET expires_at = ? WHERE principal_id = (SELECT principal_id FROM devices WHERE id = ?)", rusqlite::params![db.clock().millis(), device])?; Ok(()) }).unwrap();
    f.request("GET", "/api/v3/manage/status", b"", Some(&token))
        .error(403, "management_forbidden");
    f.host().management().revoke_local(&device).unwrap();
    f.request("GET", "/api/v3/manage/status", b"", Some(&token))
        .error(401, "management_session_invalid");
}
#[test]
fn one_session_crosses_access_domains_only_when_its_user_has_the_capability() {
    let f = Fixture::new("minecraft-or-password", 8, 1024, 10);
    let owner = f.owner();
    let pack_session = f.password_session(&owner);
    let pack_ticket = f
        .request("POST", "/api/v2/pack/ticket", b"", Some(&pack_session))
        .json()["ticket"]
        .as_str()
        .unwrap()
        .to_owned();
    f.request("GET", "/api/v3/manage/status", b"", Some(&pack_session))
        .error(403, "management_forbidden");
    f.request("GET", "/api/v3/manage/status", b"", Some(&pack_ticket))
        .error(401, "management_session_invalid");
    let (device, key) = enroll_management(&f, vesta_control::identity::BuiltinRole::Owner);
    let token = management_session(&f, &device, &key);
    f.request("GET", "/api/v3/manage/status", b"", Some(&token))
        .error(501, "not_implemented");
    assert_eq!(
        f.request("POST", "/api/v2/pack/ticket", b"", Some(&token))
            .status,
        200
    );
    assert_eq!(
        f.request("POST", "/api/v2/realtime/ticket", b"", Some(&token))
            .status,
        200
    );
    let bytes = b"same user same session";
    f.publish(&owner, UpdatePolicy::Optional, bytes, "1");
    assert_eq!(
        f.request("GET", "/api/v2/pack/artifact", b"", Some(&token))
            .body,
        bytes
    );
    f.host().management().revoke_local(&device).unwrap();
    f.request("GET", "/api/v3/manage/status", b"", Some(&token))
        .error(401, "management_session_invalid");
    f.request("GET", "/api/v2/pack/artifact", b"", Some(&token))
        .error(401, "pack_session_invalid");
}
#[test]
fn roleless_device_https_session_is_refused_on_every_management_placeholder() {
    let f = Fixture::new("minecraft", 8, 1024, 3);
    let (device, key) = enroll_management(&f, vesta_control::identity::BuiltinRole::Viewer);
    f.host().database().write(|c| { c.execute("DELETE FROM role_assignments WHERE principal_id = (SELECT principal_id FROM devices WHERE id = ?)", [&device])?; Ok(()) }).unwrap();
    let token = management_session(&f, &device, &key);
    for (method, path, body) in [
        ("GET", "/api/v3/manage", b"".as_slice()),
        ("GET", "/api/v3/manage/status", b""),
        ("POST", "/api/v3/manage/owner/link", b"{}"),
        ("POST", "/api/v3/manage/operations", b"{}"),
        ("DELETE", "/api/v3/manage/files/unknown", b""),
    ] {
        let reply = f.request(method, path, body, Some(&token));
        if path == "/api/v3/manage/operations" {
            management_reply_error(&reply, 403, "management_forbidden");
        } else {
            reply.error(403, "management_forbidden");
        }
    }
    assert!(f.host().management().links_local().unwrap().is_empty());
}
#[test]
fn management_https_body_rate_limits_strict_shapes_and_unknown_minecraft_proofs() {
    let f = Fixture::new("minecraft", 8, 256, 3);
    f.request("POST", "/api/v3/manage/pair", &vec![b'x'; 257], None)
        .error(413, "request_too_large");
    for (path, body) in [
        (
            "/api/v3/manage/pair",
            br#"{"code":"wrong","device_name":"Fixture","public_key":"wrong","role":"owner"}"#
                .as_slice(),
        ),
        (
            "/api/v3/manage/session/challenge",
            br#"{"device_id":"unknown","role":"owner"}"#,
        ),
        (
            "/api/v3/manage/session",
            br#"{"device_id":"unknown","nonce":"bad","signature":"bad","purpose":"pack"}"#,
        ),
        (
            "/api/v3/manage/minecraft/complete",
            br#"{"challenge":"bad","device_name":"Fixture","public_key":"bad","role":"owner"}"#,
        ),
    ] {
        f.request("POST", path, body, None)
            .error(400, "invalid_request");
    }
    // Unknown IDs never call Mojang. Neither a legacy ticket nor a claimed UUID is proof.
    let completion = serde_json::to_vec(&serde_json::json!({"challenge": "0".repeat(64), "device_name": "Fixture", "public_key": "A".repeat(43)})).unwrap();
    f.request(
        "POST",
        "/api/v3/manage/minecraft/complete",
        &completion,
        None,
    )
    .error(401, "management_session_invalid");
    let mut limited = false;
    for _ in 0..65 {
        let reply = f.request(
            "POST",
            "/api/v3/manage/session/challenge",
            br#"{"device_id":"unknown"}"#,
            None,
        );
        if reply.status == 429 {
            reply.error(429, "peer_rate_limited");
            limited = true;
            break;
        }
        reply.error(401, "management_session_invalid");
    }
    assert!(limited);
}

#[test]
fn public_v3_auth_aliases_and_pair_attachment_reuse_the_signed_users_session() {
    use base64::Engine as _;
    use ring::signature::KeyPair;
    let f = Fixture::new("minecraft", 8, 1024, 3);
    let key = management_key();
    let public = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(key.public_key().as_ref());
    let enrolled = f.request(
        "POST",
        "/api/v3/auth/device/enroll",
        &serde_json::to_vec(&serde_json::json!({"device_name":"Alias user","public_key":public}))
            .unwrap(),
        None,
    );
    assert_eq!(enrolled.status, 200);
    let device = enrolled.json()["device_id"].as_str().unwrap().to_owned();
    let challenge = f.request(
        "POST",
        "/api/v3/auth/session/challenge",
        &serde_json::to_vec(&serde_json::json!({"device_id":device})).unwrap(),
        None,
    );
    assert_eq!(challenge.status, 200);
    let c = challenge.json();
    let signature = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(
        key.sign(&vesta_control::identity::management::session_message(
            c["server_id"].as_str().unwrap(),
            c["nonce"].as_str().unwrap(),
        ))
        .as_ref(),
    );
    let body = serde_json::to_vec(
        &serde_json::json!({"device_id":device,"nonce":c["nonce"],"signature":signature}),
    )
    .unwrap();
    let session = f.request("POST", "/api/v3/auth/session", &body, None);
    assert_eq!(session.status, 200);
    let token = session.json()["token"].as_str().unwrap().to_owned();
    f.request("POST", "/api/v3/auth/session", &body, None)
        .error(401, "management_session_invalid");
    assert_eq!(
        f.request("GET", "/api/v2/pack/status?installed=0", b"", Some(&token))
            .status,
        200
    );
    f.request("GET", "/api/v3/manage/status", b"", Some(&token))
        .error(403, "management_forbidden");
    f.request("POST", "/api/v2/pack/ticket", b"", Some(&token))
        .error(401, "pack_session_invalid");
    let code = f
        .host()
        .management()
        .issue_local(vesta_control::identity::BuiltinRole::Owner, 10)
        .unwrap();
    let paired=f.request("POST","/api/v3/manage/pair",&serde_json::to_vec(&serde_json::json!({"code":code.secret.expose(),"device_name":"Alias user","public_key":public})).unwrap(),Some(&token));
    assert_eq!(paired.status, 200);
    assert_eq!(paired.json()["device_id"], device);
    f.request("GET", "/api/v3/manage/status", b"", Some(&token))
        .error(501, "not_implemented");
    assert_eq!(
        f.request("POST", "/api/v2/pack/ticket", b"", Some(&token))
            .status,
        200
    );
    assert_eq!(f.host().management().list_local().unwrap().len(), 1);
}
#[test]
fn anonymous_pack_is_explicit_and_existing_tickets_recheck_local_source_changes() {
    let f = Fixture::new("minecraft", 8, 1024, 3);
    let owner = f.owner();
    let bytes = b"explicit open pack";
    f.publish(&owner, UpdatePolicy::Optional, bytes, "1");
    f.request("GET", "/api/v2/pack/artifact", b"", None)
        .error(401, "pack_session_invalid");
    f.host()
        .management()
        .patch_access_local(&[("anonymous".into(), "player".into())])
        .unwrap();
    assert_eq!(
        f.request("GET", "/api/v2/pack/artifact", b"", None).body,
        bytes
    );
    let ticket = f.request("POST", "/api/v2/pack/ticket", b"", None);
    assert_eq!(ticket.status, 200);
    let ticket = ticket.json()["ticket"].as_str().unwrap().to_owned();
    f.host()
        .management()
        .patch_access_local(&[("anonymous".into(), "public".into())])
        .unwrap();
    f.request("GET", "/api/v2/pack/artifact", b"", Some(&ticket))
        .error(401, "pack_session_invalid");
    assert_eq!(
        f.request("GET", "/api/v2/pack/status?installed=0", b"", None)
            .status,
        200
    );
}

fn persisted_whitelist_user(f: &Fixture) -> String {
    // Test-only durable record for a previously verified canonical UUID. The
    // device/session uses real enrollment, Ed25519 and signed challenge flow.
    let token = f.device_session();
    let subject = f.host().management().authenticate(&token).unwrap();
    let db = f.host().database();
    db.write(|c| {
        c.execute(
            "INSERT INTO user_minecraft_accounts VALUES (?,?,?)",
            rusqlite::params![
                db.server_id(),
                "12345678-1234-4234-8234-123456789abc",
                subject.principal.as_str()
            ],
        )?;
        Ok(())
    })
    .unwrap();
    token
}
fn write_live_whitelist(f: &Fixture) {
    std::fs::write(
        f.temp.path().join("game/server.properties"),
        b"online-mode=true\nwhite-list=false\n",
    )
    .unwrap();
    std::fs::write(
        f.temp.path().join("game/whitelist.json"),
        br#"[{"uuid":"12345678-1234-4234-8234-123456789abc","name":"Fixture"}]"#,
    )
    .unwrap();
    std::fs::write(f.temp.path().join("game/banned-players.json"), b"[]").unwrap();
}
fn live_download_ticket(f: &Fixture, token: &str) -> String {
    let reply = f.request("POST", "/api/v2/pack/ticket", b"", Some(token));
    assert_eq!(reply.status, 200);
    reply.json()["ticket"].as_str().unwrap().to_owned()
}
#[test]
fn whitelist_and_ban_edits_apply_on_the_next_http_use_without_reload() {
    let f = Fixture::new("minecraft", 8, 4096, 3);
    let owner = f.owner();
    let bytes = b"live admission";
    f.publish(&owner, UpdatePolicy::Optional, bytes, "1");
    let token = persisted_whitelist_user(&f);
    write_live_whitelist(&f);
    // Startup snapshot was empty; current files alone establish access.
    assert_eq!(
        f.request("GET", "/api/v2/pack/artifact", b"", Some(&token))
            .body,
        bytes
    );
    let ticket = live_download_ticket(&f, &token);
    std::fs::write(f.temp.path().join("game/whitelist.json"), b"[]").unwrap();
    f.request("GET", "/api/v2/pack/artifact", b"", Some(&token))
        .error(401, "pack_session_invalid");
    f.request("GET", "/api/v2/pack/artifact", b"", Some(&ticket))
        .error(401, "pack_session_invalid");
    write_live_whitelist(&f);
    let ticket = live_download_ticket(&f, &token);
    std::fs::write(
        f.temp.path().join("game/banned-players.json"),
        br#"[{"uuid":"12345678-1234-4234-8234-123456789abc","name":"Fixture"}]"#,
    )
    .unwrap();
    f.request("GET", "/api/v2/pack/artifact", b"", Some(&token))
        .error(401, "pack_session_invalid");
    f.request("GET", "/api/v2/pack/artifact", b"", Some(&ticket))
        .error(401, "pack_session_invalid");
    write_live_whitelist(&f);
    assert_eq!(
        f.request("GET", "/api/v2/pack/artifact", b"", Some(&token))
            .body,
        bytes
    );
}
#[test]
fn unreadable_or_malformed_current_admission_disables_only_whitelist_source() {
    let f = Fixture::new("minecraft-or-password", 8, 4096, 3);
    let owner = f.owner();
    let bytes = b"independent sources";
    f.publish(&owner, UpdatePolicy::Optional, bytes, "1");
    let password = f.direct_session(&owner);
    let (device, key) = enroll_management(&f, vesta_control::identity::BuiltinRole::Owner);
    let explicit = management_session(&f, &device, &key);
    let token = persisted_whitelist_user(&f);
    for file in ["whitelist.json", "banned-players.json", "server.properties"] {
        write_live_whitelist(&f);
        let ticket = live_download_ticket(&f, &token);
        // Populate a valid cache too: failure must never reuse it.
        f.host().network_handle().reload_admission().unwrap();
        let corrupt = if file == "server.properties" {
            b"online-mode=invalid".as_slice()
        } else {
            b"malformed document".as_slice()
        };
        std::fs::write(f.temp.path().join("game").join(file), corrupt).unwrap();
        f.request("GET", "/api/v2/pack/artifact", b"", Some(&token))
            .error(401, "pack_session_invalid");
        f.request("GET", "/api/v2/pack/artifact", b"", Some(&ticket))
            .error(401, "pack_session_invalid");
        assert_eq!(
            f.request("GET", "/api/v2/pack/artifact", b"", Some(&password))
                .body,
            bytes
        );
        assert_eq!(
            f.request("GET", "/api/v2/pack/artifact", b"", Some(&explicit))
                .body,
            bytes
        );
        assert_eq!(
            f.request("GET", "/api/v2/pack/status?installed=0", b"", None)
                .status,
            200
        );
    }
    write_live_whitelist(&f);
    let ticket = live_download_ticket(&f, &token);
    let path = f.temp.path().join("game/whitelist.json");
    std::fs::remove_file(&path).unwrap();
    std::fs::create_dir(&path).unwrap();
    f.request("GET", "/api/v2/pack/artifact", b"", Some(&token))
        .error(401, "pack_session_invalid");
    f.request("GET", "/api/v2/pack/artifact", b"", Some(&ticket))
        .error(401, "pack_session_invalid");
    assert_eq!(
        f.request("GET", "/api/v2/pack/artifact", b"", Some(&password))
            .body,
        bytes
    );
}

fn management_reply_error(reply: &WireReply, status: u16, code: &str) {
    assert_eq!(reply.status, status);
    let value = reply.json();
    assert_eq!(value["api_version"], "3");
    assert!(value["request_id"].as_str().unwrap().len() < 128);
    assert_eq!(value["error"]["code"], code);
    assert!(value.get("data").is_none());
}

#[test]
fn management_read_projections_are_bounded_and_do_not_invent_players() {
    let f = Fixture::new("minecraft", 8, 4096, 3);
    let (device, key) = enroll_management(&f, vesta_control::identity::BuiltinRole::Owner);
    let token = management_session(&f, &device, &key);
    let status = f.request("GET", "/api/v3/manage/server/status", b"", Some(&token));
    assert_eq!(status.status, 200);
    let data = status.json()["data"].clone();
    assert_eq!(data["state"], "stopped");
    assert!(data["generation"].is_null());
    assert!(data["player_count"].is_null());
    assert_eq!(data["player_count_known"], false);
    assert!(data["uptime_seconds"].is_null());
    let players = f.request("GET", "/api/v3/manage/players", b"", Some(&token));
    assert_eq!(players.status, 200);
    assert_eq!(players.json()["data"]["known"], false);
    assert!(players.json()["data"]["player_count"].is_null());
    let pack = f.request("GET", "/api/v3/manage/pack", b"", Some(&token));
    assert_eq!(pack.json()["data"]["status"], "unpublished");
    let server = f.request("GET", "/api/v3/manage/server", b"", Some(&token));
    assert_eq!(server.json()["data"]["name"], "Hosted");
    for path in ["console", "audit", "operations"] {
        let reply = f.request(
            "GET",
            &format!("/api/v3/manage/{path}?limit=1"),
            b"",
            Some(&token),
        );
        assert_eq!(reply.status, 200);
        assert!(reply.json()["data"]["items"].as_array().unwrap().len() <= 1);
        reply.hides(&f.temp.path().to_string_lossy());
        reply.hides(&token);
        management_reply_error(
            &f.request(
                "GET",
                &format!("/api/v3/manage/{path}?limit=201"),
                b"",
                Some(&token),
            ),
            400,
            "invalid_request",
        );
        management_reply_error(
            &f.request(
                "GET",
                &format!("/api/v3/manage/{path}?cursor=another-server.audit.0"),
                b"",
                Some(&token),
            ),
            400,
            "invalid_request",
        );
    }
    let audit = f.request("GET", "/api/v3/manage/audit", b"", Some(&token));
    for item in audit.json()["data"]["items"].as_array().unwrap() {
        assert!(item.get("target").is_none());
        assert!(item.get("metadata").is_none());
        assert!(item.get("principal_id").is_none());
        assert!(item.get("device_id").is_none());
    }
}

#[test]
fn management_operational_endpoints_refuse_anonymous_and_pack_only_devices() {
    let f = Fixture::new("minecraft", 8, 4096, 3);
    let mut sources = f.host().management().access_local().unwrap();
    sources.new_user = "player".into();
    f.host().management().set_access_local(sources).unwrap();
    let token = f.device_session();
    for (method, path) in [
        ("GET", "server"),
        ("GET", "server/status"),
        ("POST", "lifecycle/start"),
        ("POST", "lifecycle/stop"),
        ("POST", "lifecycle/restart"),
        ("POST", "lifecycle/kill"),
        ("GET", "operations"),
        ("GET", "operations/00000000-0000-4000-8000-000000000000"),
        ("GET", "console"),
        ("GET", "players"),
        ("GET", "pack"),
        ("GET", "audit"),
        ("POST", "console/command"),
        ("POST", "pack/publish"),
        ("POST", "pack/minimum"),
    ] {
        let path = format!("/api/v3/manage/{path}");
        let body: &[u8] = if method == "POST" { b"{}" } else { b"" };
        management_reply_error(
            &f.request(method, &path, body, None),
            401,
            "management_session_invalid",
        );
        management_reply_error(
            &f.request(method, &path, body, Some(&token)),
            403,
            "management_forbidden",
        );
    }
}

#[test]
fn management_each_endpoint_checks_its_exact_capability_not_status_gate() {
    use vesta_control::identity::{BuiltinRole, Capability};
    for (method, path, capability) in [
        ("GET", "server", Capability::StatusView),
        ("GET", "server/status", Capability::StatusView),
        ("POST", "lifecycle/start", Capability::LifecycleStart),
        ("POST", "lifecycle/stop", Capability::LifecycleStop),
        ("POST", "lifecycle/restart", Capability::LifecycleRestart),
        ("POST", "lifecycle/kill", Capability::LifecycleForceKill),
        ("GET", "operations", Capability::TasksView),
        (
            "GET",
            "operations/00000000-0000-4000-8000-000000000000",
            Capability::TasksView,
        ),
        ("GET", "console", Capability::ConsoleRead),
        ("GET", "players", Capability::PlayersView),
        ("GET", "pack", Capability::StatusView),
        ("GET", "audit", Capability::AuditView),
        ("POST", "console/command", Capability::ConsoleWrite),
        ("POST", "pack/publish", Capability::PackPublish),
        ("POST", "pack/minimum", Capability::PackForceUpdate),
    ] {
        let f = Fixture::new("minecraft", 8, 4096, 3);
        let (device, key) = enroll_management(&f, BuiltinRole::Owner);
        let token = management_session(&f, &device, &key);
        let db = f.host().database();
        db.write(|c| {
            c.execute("INSERT INTO principal_overrides (id,principal_id,capability,scope,effect,granted_by,created_at) VALUES ('11111111-2222-4333-8444-555555555555',(SELECT principal_id FROM devices WHERE id=?),?,'*','deny','fixture',?)", rusqlite::params![device, capability.id(), db.clock().millis()])?;
            Ok(())
        }).unwrap();
        management_reply_error(
            &f.request(
                method,
                &format!("/api/v3/manage/{path}"),
                if method == "POST" { b"{}" } else { b"" },
                Some(&token),
            ),
            403,
            "management_forbidden",
        );
        db.write(|c| {
            c.execute("DELETE FROM principal_overrides", [])?;
            c.execute("INSERT INTO roles (id,name,rank,builtin,created_by,created_at) VALUES ('capability-absence','Capability absence',1,0,'fixture',?)", [db.clock().millis()])?;
            c.execute("INSERT INTO role_capabilities (role_id,capability,scope) SELECT 'capability-absence',capability,scope FROM role_capabilities WHERE role_id='owner' AND capability<>?", [capability.id()])?;
            c.execute("UPDATE role_assignments SET role_id='capability-absence' WHERE principal_id=(SELECT principal_id FROM devices WHERE id=?)", [&device])?;
            Ok(())
        })
        .unwrap();
        management_reply_error(
            &f.request(
                method,
                &format!("/api/v3/manage/{path}"),
                if method == "POST" { b"{}" } else { b"" },
                Some(&token),
            ),
            403,
            "management_forbidden",
        );
    }
}

#[test]
fn management_lifecycle_admission_is_idempotent_and_inputs_are_not_launch_authority() {
    let f = Fixture::new("minecraft", 8, 4096, 3);
    let (device, key) = enroll_management(&f, vesta_control::identity::BuiltinRole::Owner);
    let token = management_session(&f, &device, &key);
    let send = |body: &[u8]| {
        let mut stream = f.stream();
        write!(stream, "POST /api/v3/manage/lifecycle/start HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\nAuthorization: Bearer {token}\r\nIdempotency-Key: test-mutation-0001\r\n\r\n", body.len()).unwrap();
        stream.write_all(body).unwrap();
        stream.flush().unwrap();
        read_reply(stream).unwrap()
    };
    management_reply_error(
        &f.request(
            "POST",
            "/api/v3/manage/lifecycle/start",
            b"{}",
            Some(&token),
        ),
        400,
        "invalid_request",
    );
    for body in [
        br#"{"argv":[]}"#.as_slice(),
        b"[]",
        br#"{"executable":"java"}"#,
    ] {
        management_reply_error(&send(body), 400, "invalid_request");
    }
    let first = send(b"{}");
    assert_eq!(first.status, 202);
    let second = send(b"{}");
    assert_eq!(second.status, 202);
    assert_eq!(
        first.json()["data"]["operation_id"],
        second.json()["data"]["operation_id"]
    );
    let id = first.json()["data"]["operation_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let outcome = f.request(
        "GET",
        &format!("/api/v3/manage/operations/{id}"),
        b"",
        Some(&token),
    );
    assert_eq!(outcome.status, 200);
    assert!(outcome.json()["data"]["progress_percent"].as_u64().unwrap() <= 100);
    outcome.hides(&f.temp.path().to_string_lossy());
}

#[test]
fn management_audit_tail_withholds_untrusted_actor_target_and_payload_fields() {
    use vesta_state::audit::{AuditActor, AuditLog, AuditRecord};
    let f = Fixture::new("minecraft", 8, 4096, 3);
    let (device, key) = enroll_management(&f, vesta_control::identity::BuiltinRole::Owner);
    let token = management_session(&f, &device, &key);
    let marker = "private-path-and-sensitive-value";
    let audit = AuditLog::new(f.host().database().clone()).unwrap();
    audit
        .append_record(
            &AuditRecord::new(
                AuditActor::system(marker.into()).unwrap(),
                marker.into(),
                marker.into(),
                marker.into(),
                None,
                [("detail".into(), Some(marker.into()))],
            )
            .unwrap(),
        )
        .unwrap();
    let reply = f.request("GET", "/api/v3/manage/audit?limit=1", b"", Some(&token));
    assert_eq!(reply.status, 200);
    reply.hides(marker);
    let value = reply.json();
    let item = &value["data"]["items"][0];
    assert_eq!(item["action"], "other");
    assert_eq!(item["outcome"], "other");
    let next = value["data"]["next_cursor"].as_str().unwrap();
    let empty = f.request(
        "GET",
        &format!("/api/v3/manage/audit?cursor={next}&limit=1"),
        b"",
        Some(&token),
    );
    assert_eq!(empty.status, 200);
    assert!(empty.json()["data"]["items"].as_array().unwrap().is_empty());
}

#[test]
fn management_stale_kill_and_stop_refuse_before_operation_admission() {
    let f = Fixture::new("minecraft", 8, 4096, 3);
    let (device, key) = enroll_management(&f, vesta_control::identity::BuiltinRole::Owner);
    let token = management_session(&f, &device, &key);
    let send = |action: &str, body: &[u8]| {
        let mut stream = f.stream();
        write!(stream, "POST /api/v3/manage/lifecycle/{action} HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\nAuthorization: Bearer {token}\r\nIdempotency-Key: test-stale-kill-0001\r\n\r\n", body.len()).unwrap();
        stream.write_all(body).unwrap();
        stream.flush().unwrap();
        read_reply(stream).unwrap()
    };
    let generation = "00112233445566778899aabbccddeeff";
    management_reply_error(
        &send(
            "kill",
            &serde_json::to_vec(
                &serde_json::json!({"expected_generation":generation,"acknowledgement":"yes"}),
            )
            .unwrap(),
        ),
        400,
        "confirmation_required",
    );
    management_reply_error(&send("kill", &serde_json::to_vec(&serde_json::json!({"expected_generation":generation,"acknowledgement":format!("kill game:generation:{generation}")})).unwrap()), 412, "precondition_failed");
    for action in ["stop", "restart"] {
        management_reply_error(
            &send(
                action,
                &serde_json::to_vec(&serde_json::json!({"expected_generation":generation}))
                    .unwrap(),
            ),
            412,
            "precondition_failed",
        );
    }
    let count: i64 = f
        .host()
        .database()
        .read(|c| Ok(c.query_row("SELECT count(*) FROM operations", [], |row| row.get(0))?))
        .unwrap();
    assert_eq!(count, 0);
}

#[test]
fn management_inert_child() {
    if std::fs::read("tls-inert-enabled").ok().as_deref() != Some(b"vesta-owned-tls-inert-v1") {
        return;
    }
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for line in std::io::stdin().lock().lines() {
            if sender.send(line.unwrap()).is_err() {
                break;
            }
        }
    });
    println!("owned-tls-child-ready");
    std::io::stdout().flush().unwrap();
    std::fs::write("tls-inert-ready", b"ready").unwrap();
    loop {
        if std::fs::read("tls-inert-release").is_ok() {
            std::process::exit(0);
        }
        match receiver.recv_timeout(Duration::from_millis(10)) {
            Ok(line) if line == "stop" => std::process::exit(0),
            Ok(line) if line == "list" => {
                println!("There are 2 of a max of 20 players online: OwnedAlpha, OwnedBeta");
                std::io::stdout().flush().unwrap();
            }
            Ok(line) if line.starts_with("owned-credential-boundaries ") => {
                let token = line.strip_prefix("owned-credential-boundaries ").unwrap();
                assert_eq!(token.len(), 43);
                for offset in [1, 7, 21, 42] {
                    let marker = format!(" boundary-{offset}: ");
                    let fill = ".".repeat(16_384 - marker.len() - offset);
                    println!("{marker}{fill}{token} :discarded-after-boundary");
                    println!(" split-{offset}: {}", &token[..offset]);
                    println!("{} :split-end-{offset} ", &token[offset..]);
                }
                std::io::stdout().flush().unwrap();
                std::fs::write("tls-inert-boundaries-ready", b"ready").unwrap();
            }
            Ok(line) => {
                println!("received {line}");
                std::io::stdout().flush().unwrap();
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => (),
            Err(_) => std::process::exit(0),
        }
    }
}
fn management_mutation(
    f: &Fixture,
    token: &str,
    path: &str,
    key: &str,
    body: &serde_json::Value,
) -> WireReply {
    let body = serde_json::to_vec(body).unwrap();
    let mut stream = f.stream();
    write!(stream,"POST /api/v3/manage/{path} HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\nAuthorization: Bearer {token}\r\nIdempotency-Key: {key}\r\n\r\n",body.len()).unwrap();
    stream.write_all(&body).unwrap();
    stream.flush().unwrap();
    read_reply(stream).unwrap()
}
fn management_wait_operation(f: &Fixture, token: &str, id: &str) {
    let until = Instant::now() + Duration::from_secs(15);
    loop {
        let reply = f.request(
            "GET",
            &format!("/api/v3/manage/operations/{id}"),
            b"",
            Some(token),
        );
        assert_eq!(reply.status, 200);
        let value = reply.json();
        let state = value["data"]["state"].as_str().unwrap();
        if state == "succeeded" {
            return;
        }
        assert!(
            !matches!(state, "failed" | "recovery_required" | "cancelled"),
            "unexpected operation outcome {state}"
        );
        assert!(Instant::now() < until, "operation timeout");
        std::thread::sleep(Duration::from_millis(100));
    }
}
fn management_start_inert(f: &Fixture, token: &str) -> String {
    let start = management_mutation(
        f,
        token,
        "lifecycle/start",
        "owned-tls-start-0001",
        &serde_json::json!({}),
    );
    assert_eq!(start.status, 202);
    management_wait_operation(
        f,
        token,
        start.json()["data"]["operation_id"].as_str().unwrap(),
    );
    let until = Instant::now() + Duration::from_secs(10);
    while !f.temp.path().join("game/tls-inert-ready").exists() {
        assert!(Instant::now() < until);
        std::thread::sleep(Duration::from_millis(10));
    }
    let status = f.request("GET", "/api/v3/manage/server/status", b"", Some(token));
    assert_eq!(status.json()["data"]["state"], "running");
    status.json()["data"]["generation"]
        .as_str()
        .unwrap()
        .to_owned()
}
fn assert_credential_boundary_lines(lines: &[serde_json::Value]) {
    for offset in [1, 7, 21, 42] {
        let marker = format!(" boundary-{offset}: ");
        let line = lines
            .iter()
            .find(|line| {
                line["text"]
                    .as_str()
                    .is_some_and(|text| text.starts_with(&marker))
            })
            .unwrap();
        assert_eq!(line["truncated"], true);
        let expected = format!(
            "{marker}{}[redacted]",
            ".".repeat(16_384 - marker.len() - offset)
        );
        assert_eq!(line["text"].as_str().unwrap(), expected);
        assert!(
            lines
                .iter()
                .any(|line| line["text"] == format!(" split-{offset}: [redacted]"))
        );
        assert!(
            lines
                .iter()
                .any(|line| line["text"] == format!("[redacted] :split-end-{offset} "))
        );
    }
}

#[test]
fn management_console_redacts_truncated_and_split_live_credentials_over_tls_and_websocket() {
    let f = Fixture::inert();
    let (device, key) = enroll_management(&f, vesta_control::identity::BuiltinRole::Owner);
    let token = management_session(&f, &device, &key);
    let generation = management_start_inert(&f, &token);
    let command = management_mutation(
        &f,
        &token,
        "console/command",
        "owned-boundary-command-0001",
        &serde_json::json!({"expected_generation":generation,"line":format!("owned-credential-boundaries {token}")}),
    );
    assert_eq!(command.status, 202);
    management_wait_operation(
        &f,
        &token,
        command.json()["data"]["operation_id"].as_str().unwrap(),
    );
    let until = Instant::now() + Duration::from_secs(10);
    let lines = loop {
        let reply = f.request("GET", "/api/v3/manage/console?limit=200", b"", Some(&token));
        assert_eq!(reply.status, 200);
        reply.hides(&token);
        let lines = reply.json()["data"]["items"].as_array().unwrap().clone();
        if lines
            .iter()
            .filter(|line| {
                line["text"]
                    .as_str()
                    .is_some_and(|text| text.contains("boundary-") || text.contains("split-"))
            })
            .count()
            == 12
        {
            break lines;
        }
        assert!(Instant::now() < until, "boundary capture timeout");
        std::thread::sleep(Duration::from_millis(50));
    };
    assert_credential_boundary_lines(&lines);
    let mut peer = realtime::console_websocket(&f, &token).unwrap();
    let mut streamed = Vec::new();
    for _ in 0..32 {
        match peer.read().unwrap() {
            tokio_tungstenite::tungstenite::Message::Text(text) => {
                assert!(!text.contains(&token));
                let value: serde_json::Value = serde_json::from_str(&text).unwrap();
                if value["type"] == "console.line" {
                    streamed.push(value);
                    if streamed
                        .iter()
                        .filter(|line| {
                            line["text"].as_str().is_some_and(|text| {
                                text.contains("boundary-") || text.contains("split-")
                            })
                        })
                        .count()
                        == 12
                    {
                        break;
                    }
                }
            }
            tokio_tungstenite::tungstenite::Message::Ping(payload) => peer
                .send(tokio_tungstenite::tungstenite::Message::Pong(payload))
                .unwrap(),
            _ => (),
        }
    }
    assert_credential_boundary_lines(&streamed);
    peer.close(None).unwrap();
    let stop = management_mutation(
        &f,
        &token,
        "lifecycle/stop",
        "owned-boundary-stop-0001",
        &serde_json::json!({"expected_generation":generation}),
    );
    assert_eq!(stop.status, 202);
    management_wait_operation(
        &f,
        &token,
        stop.json()["data"]["operation_id"].as_str().unwrap(),
    );
}

#[test]
fn management_tls_inert_lifecycle_command_console_and_known_players() {
    let f = Fixture::inert();
    let (device, key) = enroll_management(&f, vesta_control::identity::BuiltinRole::Owner);
    let token = management_session(&f, &device, &key);
    let generation = management_start_inert(&f, &token);
    let players = f.request("GET", "/api/v3/manage/players", b"", Some(&token));
    assert_eq!(players.json()["data"]["player_count"], 2);
    let status = f.request("GET", "/api/v3/manage/server/status", b"", Some(&token));
    assert_eq!(status.json()["data"]["player_count"], 2);
    assert!(
        status.json()["data"]["player_count_observed_at_ms"]
            .as_i64()
            .unwrap()
            > 0
    );
    for line in ["", "say first\nstop", "say first\rstop", &"x".repeat(1025)] {
        management_reply_error(
            &management_mutation(
                &f,
                &token,
                "console/command",
                "owned-tls-invalid-001",
                &serde_json::json!({"expected_generation":generation,"line":line}),
            ),
            400,
            "invalid_request",
        );
    }
    let local_code = f
        .host()
        .management()
        .issue_local(vesta_control::identity::BuiltinRole::Viewer, 1)
        .unwrap();
    let code = local_code.secret.expose().to_owned();
    let path = f
        .host()
        .configuration()
        .state()
        .to_string_lossy()
        .into_owned();
    let body = serde_json::json!({"expected_generation":generation,"line":format!("say marker-actual-output {token} {path} {code}")});
    let command = management_mutation(
        &f,
        &token,
        "console/command",
        "owned-tls-command-001",
        &body,
    );
    assert_eq!(command.status, 202);
    let id = command.json()["data"]["operation_id"]
        .as_str()
        .unwrap()
        .to_owned();
    management_wait_operation(&f, &token, &id);
    let retry = management_mutation(
        &f,
        &token,
        "console/command",
        "owned-tls-command-001",
        &body,
    );
    assert_eq!(retry.json()["data"]["operation_id"], id);
    let until = Instant::now() + Duration::from_secs(5);
    loop {
        let console = f.request("GET", "/api/v3/manage/console?limit=200", b"", Some(&token));
        console.hides(&token);
        console.hides(&path);
        console.hides(&code);
        if console.json()["data"]["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| {
                v["text"]
                    .as_str()
                    .unwrap()
                    .contains("marker-actual-output [redacted] [redacted]")
            })
        {
            break;
        }
        assert!(Instant::now() < until);
        std::thread::sleep(Duration::from_millis(100));
    }
    let audit = f.request("GET", "/api/v3/manage/audit?limit=200", b"", Some(&token));
    audit.hides(&token);
    audit.hides("marker-actual-output");
    assert!(
        audit.json()["data"]["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v["action"] == "console.command" && v["outcome"] == "succeeded")
    );
    let raw_count=f.host().database().read(|c| Ok(c.query_row("SELECT count(*) FROM audit_events WHERE instr(metadata_json,'marker-actual-output') > 0",[],|r|r.get::<_,i64>(0))?)).unwrap();
    assert_eq!(raw_count, 0, "command text must not enter audit metadata");
    let mut peer = realtime::console_websocket(&f, &token).unwrap();
    let mut saw_line = false;
    for _ in 0..24 {
        let message = peer.read().unwrap();
        if let tokio_tungstenite::tungstenite::Message::Text(text) = message {
            assert!(!text.contains(&token));
            assert!(!text.contains(&path));
            assert!(!text.contains(&code));
            assert!(text.len() <= 128 * 1024);
            let value: serde_json::Value = serde_json::from_str(&text).unwrap();
            if value["type"] == "console.line"
                && value["text"]
                    .as_str()
                    .unwrap()
                    .contains("marker-actual-output [redacted] [redacted]")
            {
                saw_line = true;
                break;
            }
        }
    }
    assert!(saw_line, "stream must expose actual redacted console text");
    peer.close(None).unwrap();
    drop(peer);
    let stop = management_mutation(
        &f,
        &token,
        "lifecycle/stop",
        "owned-tls-stop-00001",
        &serde_json::json!({"expected_generation":generation}),
    );
    assert_eq!(stop.status, 202);
    management_wait_operation(
        &f,
        &token,
        stop.json()["data"]["operation_id"].as_str().unwrap(),
    );
    let status = f.request("GET", "/api/v3/manage/server/status", b"", Some(&token));
    assert_eq!(status.json()["data"]["state"], "stopped");
    assert!(status.json()["data"]["player_count"].is_null());
    management_reply_error(
        &management_mutation(
            &f,
            &token,
            "console/command",
            "owned-tls-stopped-001",
            &body,
        ),
        412,
        "precondition_failed",
    );
}
#[test]
fn management_tls_inert_kill_requires_exact_confirmation() {
    let f = Fixture::inert();
    let (device, key) = enroll_management(&f, vesta_control::identity::BuiltinRole::Owner);
    let token = management_session(&f, &device, &key);
    let generation = management_start_inert(&f, &token);
    let mut body = serde_json::json!({"expected_generation":generation,"acknowledgement":format!("kill game:generation:{generation}")});
    let preview = management_mutation(&f, &token, "lifecycle/kill", "owned-tls-kill-00001", &body);
    assert_eq!(preview.status, 202);
    assert_eq!(
        f.request("GET", "/api/v3/manage/server/status", b"", Some(&token))
            .json()["data"]["state"],
        "running"
    );
    body["confirmation_token"] = preview.json()["data"]["confirmation_token"].clone();
    let confirmed =
        management_mutation(&f, &token, "lifecycle/kill", "owned-tls-kill-00001", &body);
    assert_eq!(confirmed.status, 202);
    let id = confirmed.json()["data"]["operation_id"]
        .as_str()
        .unwrap()
        .to_owned();
    management_wait_operation(&f, &token, &id);
    assert_eq!(
        f.request("GET", "/api/v3/manage/server/status", b"", Some(&token))
            .json()["data"]["state"],
        "stopped"
    );
    assert_eq!(
        management_mutation(&f, &token, "lifecycle/kill", "owned-tls-kill-00001", &body).json()["data"]
            ["operation_id"],
        id
    );
}
#[test]
fn management_pack_mutations_are_authorized_atomic_and_idempotent() {
    let f = Fixture::new("minecraft", 8, 4096, 5);
    let (device, key) = enroll_management(&f, vesta_control::identity::BuiltinRole::Owner);
    let token = management_session(&f, &device, &key);
    std::fs::write(
        f.temp.path().join("game/config/owned.cfg"),
        b"owned pack bytes",
    )
    .unwrap();
    let subject = f
        .host()
        .management()
        .authorize(
            &token,
            vesta_control::identity::Capability::PackPublish,
            None,
        )
        .unwrap();
    let plan = f
        .host()
        .releases()
        .plan(&subject, 0, UpdatePolicy::Optional)
        .unwrap();
    f.host()
        .prepare_pack(
            &plan,
            &vesta_protocol::pack::sources::SourceRegistry::default(),
        )
        .unwrap();
    let publish = serde_json::json!({"expected_revision":"0"});
    let first = management_mutation(
        &f,
        &token,
        "pack/publish",
        "owned-pack-publish-01",
        &publish,
    );
    assert_eq!(first.status, 200);
    assert_eq!(first.json()["data"]["hosted_revision"], "1");
    assert_eq!(
        management_mutation(
            &f,
            &token,
            "pack/publish",
            "owned-pack-publish-01",
            &publish
        )
        .json()["data"],
        first.json()["data"]
    );
    let second = management_mutation(
        &f,
        &token,
        "pack/publish",
        "owned-pack-publish-02",
        &serde_json::json!({"expected_revision":"1"}),
    );
    assert_eq!(second.status, 200);
    f.host().database().write(|c| { c.execute("INSERT INTO principal_overrides(id,principal_id,capability,scope,effect,granted_by,created_at) VALUES(?,?,?,'*','deny','owned-test',?)",["owned-pack-publish-deny".to_owned(),subject.principal.as_str().to_owned(),vesta_control::identity::Capability::PackPublish.id().to_owned(),f.host().database().clock().millis().to_string()])?; Ok(()) }).unwrap();
    management_reply_error(
        &management_mutation(
            &f,
            &token,
            "pack/publish",
            "owned-pack-denied-001",
            &serde_json::json!({"expected_revision":"2"}),
        ),
        403,
        "management_forbidden",
    );
    let minimum = serde_json::json!({"expected_revision":"2","minimum_revision":"2"});
    f.host().database().write(|c| { c.execute_batch("CREATE TRIGGER owned_minimum_audit_failure BEFORE INSERT ON audit_events WHEN NEW.action='pack_release.minimum' BEGIN SELECT RAISE(ABORT,'owned fixture'); END;")?; Ok(()) }).unwrap();
    management_reply_error(
        &management_mutation(
            &f,
            &token,
            "pack/minimum",
            "owned-pack-minimum-01",
            &minimum,
        ),
        503,
        "temporarily_unavailable",
    );
    assert_eq!(
        f.host()
            .releases()
            .current()
            .unwrap()
            .unwrap()
            .minimum_revision(),
        1
    );
    let before = f
        .host()
        .database()
        .read(|c| {
            Ok(
                c.query_row("SELECT count(*) FROM management_pack_outcomes", [], |r| {
                    r.get::<_, i64>(0)
                })?,
            )
        })
        .unwrap();
    assert_eq!(before, 2);
    f.host()
        .database()
        .write(|c| {
            c.execute_batch("DROP TRIGGER owned_minimum_audit_failure;")?;
            Ok(())
        })
        .unwrap();
    let raised = management_mutation(
        &f,
        &token,
        "pack/minimum",
        "owned-pack-minimum-01",
        &minimum,
    );
    assert_eq!(raised.status, 200);
    assert_eq!(raised.json()["data"]["minimum_revision"], "2");
    assert_eq!(
        management_mutation(
            &f,
            &token,
            "pack/minimum",
            "owned-pack-minimum-01",
            &minimum
        )
        .json()["data"],
        raised.json()["data"]
    );
    assert_eq!(
        f.request("GET", "/api/v3/manage/pack", b"", Some(&token))
            .json()["data"]["minimum_revision"],
        "2"
    );
    let audit = f.request("GET", "/api/v3/manage/audit?limit=200", b"", Some(&token));
    assert!(
        audit.json()["data"]["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v["action"] == "pack_release.minimum" && v["outcome"] == "success")
    );
    management_reply_error(
        &management_mutation(
            &f,
            &token,
            "pack/minimum",
            "owned-pack-minimum-01",
            &serde_json::json!({"expected_revision":"2","minimum_revision":"1"}),
        ),
        409,
        "idempotency_conflict",
    );
    management_reply_error(
        &management_mutation(
            &f,
            &token,
            "pack/minimum",
            "owned-pack-minimum-02",
            &serde_json::json!({"expected_revision":"2","minimum_revision":"1"}),
        ),
        412,
        "precondition_failed",
    );
    let counts = f
        .host()
        .database()
        .read(|c| {
            Ok((
                c.query_row("SELECT count(*) FROM pack_releases", [], |r| {
                    r.get::<_, i64>(0)
                })?,
                c.query_row("SELECT count(*) FROM management_pack_outcomes", [], |r| {
                    r.get::<_, i64>(0)
                })?,
            ))
        })
        .unwrap();
    assert_eq!(counts, (2, 3));
}

#[path = "support/management_files.rs"]
mod management_files;

#[path = "support/large_files.rs"]
mod large_files;

#[path = "support/management_permissions.rs"]
mod management_permissions;
