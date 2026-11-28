use super::*;
use ring::signature::{Ed25519KeyPair, KeyPair};
use rustls::{ClientConfig, ClientConnection, RootCertStore, StreamOwned};
use rustls_pki_types::{ServerName, pem::PemObject};
use std::{
    io::{BufRead, BufReader, Read, Write},
    net::TcpStream,
    path::Path,
};
use vesta_control::config::{HostConfiguration, ListenerDescription};
use vesta_control::machine::{
    HostState,
    recovery::LocalPrivilege,
    registry::{ServerEntry, ServerState, Visibility},
};
use vesta_control::worker::Worker;
use vesta_host_client::{Client, Finished, Purpose, SetupCode, Started, encode};
struct Wire(
    BufReader<StreamOwned<ClientConnection, TcpStream>>,
    Option<String>,
    Option<u64>,
);
impl Wire {
    fn connect(address: SocketAddr) -> Self {
        let mut roots = RootCertStore::empty();
        roots
            .add(
                rustls_pki_types::CertificateDer::from_pem_slice(include_bytes!(
                    "../../tests/fixtures/test-only-ca.pem"
                ))
                .unwrap(),
            )
            .unwrap();
        let mut config =
            ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
                .with_protocol_versions(&[&rustls::version::TLS13])
                .unwrap()
                .with_root_certificates(roots)
                .with_no_client_auth();
        config.resumption = rustls::client::Resumption::disabled();
        let socket = TcpStream::connect(address).unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let mut stream = StreamOwned::new(
            ClientConnection::new(Arc::new(config), ServerName::try_from("localhost").unwrap())
                .unwrap(),
            socket,
        );
        while stream.conn.is_handshaking() {
            stream.conn.complete_io(&mut stream.sock).unwrap();
        }
        Self(BufReader::new(stream), None, None)
    }
    fn request(
        &mut self,
        method: &str,
        path: &str,
        body: Value,
        authorization: Option<&str>,
    ) -> (u16, Value) {
        let bytes = if body.is_null() {
            Vec::new()
        } else {
            serde_json::to_vec(&body).unwrap()
        };
        let auth = authorization
            .map(|a| format!("Authorization: {a}\r\n"))
            .unwrap_or_default();
        write!(self.0.get_mut(),"{method} {path} HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\n{auth}\r\n",bytes.len()).unwrap();
        self.0.get_mut().write_all(&bytes).unwrap();
        self.0.get_mut().flush().unwrap();
        let mut line = String::new();
        self.0.read_line(&mut line).unwrap();
        let status = line.split_whitespace().nth(1).unwrap().parse().unwrap();
        let mut len = 0;
        self.1 = None;
        self.2 = None;
        loop {
            line.clear();
            self.0.read_line(&mut line).unwrap();
            if line == "\r\n" {
                break;
            }
            if let Some((k, v)) = line.split_once(':') {
                if k.eq_ignore_ascii_case("etag") {
                    self.1 = Some(v.trim().to_owned());
                }
                if k.eq_ignore_ascii_case("content-length") {
                    len = v.trim().parse().unwrap();
                }
                if k.eq_ignore_ascii_case("retry-after") {
                    self.2 = Some(v.trim().parse().unwrap());
                }
            }
        }
        let mut body = vec![0; len];
        self.0.read_exact(&mut body).unwrap();
        (status, serde_json::from_slice(&body).unwrap_or(Value::Null))
    }
    /// Raw body exchange; `headers` are complete `Name: value\r\n` lines.
    /// Returns the status, the raw body and any ETag.
    fn raw(
        &mut self,
        method: &str,
        path: &str,
        headers: &str,
        body: &[u8],
    ) -> (u16, Vec<u8>, Option<String>) {
        write!(
            self.0.get_mut(),
            "{method} {path} HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n{headers}\r\n",
            body.len()
        )
        .unwrap();
        self.0.get_mut().write_all(body).unwrap();
        self.0.get_mut().flush().unwrap();
        let mut line = String::new();
        self.0.read_line(&mut line).unwrap();
        let status = line.split_whitespace().nth(1).unwrap().parse().unwrap();
        let (mut len, mut etag) = (0, None);
        self.2 = None;
        loop {
            line.clear();
            self.0.read_line(&mut line).unwrap();
            if line == "\r\n" {
                break;
            }
            if let Some((k, v)) = line.split_once(':') {
                if k.eq_ignore_ascii_case("etag") {
                    etag = Some(v.trim().to_owned());
                }
                if k.eq_ignore_ascii_case("content-length") {
                    len = v.trim().parse().unwrap();
                }
                if k.eq_ignore_ascii_case("retry-after") {
                    self.2 = Some(v.trim().parse().unwrap());
                }
            }
        }
        let mut out = vec![0; len];
        self.0.read_exact(&mut out).unwrap();
        (status, out, etag)
    }
}
use serde_json::Value;
fn signed(key: &Ed25519KeyPair, domain: &str, payload: Value) -> String {
    encode(
        key.sign(&vesta_host_client::requests::signing_bytes(
            domain, &payload,
        ))
        .as_ref(),
    )
}
fn session(wire: &mut Wire, host: Uuid, device: Uuid, key: &Ed25519KeyPair) -> String {
    let (status, c) = wire.request(
        "POST",
        "/v1/host/sessions/challenge",
        json!({"deviceId":device}),
        None,
    );
    assert_eq!(status, 200, "{c}");
    let signature = signed(
        key,
        "vesta.host.session.v1",
        json!({"hostId":host,"deviceId":device,"challengeId":c["challengeId"],"nonce":c["nonce"],"expiresAt":c["expiresAt"]}),
    );
    let (status, s) = wire.request(
        "POST",
        "/v1/host/sessions",
        json!({"deviceId":device,"challengeId":c["challengeId"],"signature":signature}),
        None,
    );
    assert_eq!(status, 200, "{s}");
    s["session"].as_str().unwrap().into()
}
const FIXTURE_CONFIG: &str = "schema-version = 1
[paths]
state = 'worker-state'
game = 'game'
scan-roots = ['game/config']
launch-definition = 'private/launch.toml'
[pack]
name = 'Worker'
version = '1.0.0'
revision = 1
minimum-revision = 1
minecraft-version = '1.21.1'
unresolved-disposition = 'embed-public'
[pack-access]
mode = 'minecraft'
";

fn fixture_listener(base: &Path) -> TlsListener {
    std::fs::write(
        base.join("cert.pem"),
        include_bytes!("../../tests/fixtures/test-only-server.pem"),
    )
    .unwrap();
    std::fs::write(
        base.join("key.pem"),
        include_bytes!("../../tests/fixtures/test-only-server-key.pem"),
    )
    .unwrap();
    // The registry end-to-end scenario sends over 60 requests from one
    // loopback peer. Give it enough permits to run without timed refills.
    let document = format!(
        "{FIXTURE_CONFIG}[listener]
mode = 'tls'
bind = '127.0.0.1'
port = 25500
certificate = 'cert.pem'
private-key = 'key.pem'
max-connections = 64
max-request-bytes = 1048576
request-timeout-seconds = 30
peer-requests-per-minute = 120
"
    );
    let config =
        HostConfiguration::parse_toml(document.as_bytes(), base, Path::new("server.toml")).unwrap();
    let ListenerDescription::Tls(listener) = config.listener() else {
        panic!("expected TLS fixture listener");
    };
    listener.on_ephemeral_port()
}

fn fixture_worker(base: &Path, host: Uuid, server: Uuid) -> Worker {
    for dir in ["game/config", "private", "bin"] {
        std::fs::create_dir_all(base.join(dir)).unwrap();
    }
    std::fs::write(base.join("bin/inert"), b"").unwrap();
    std::fs::write(base.join("private/launch.toml"),"[minecraft]\nworking-directory = 'game'\n[launch]\nkind = 'native'\nexecutable = '../bin/inert'\nstop = 'console-stop'\n").unwrap();
    std::fs::write(
        base.join("game/config/test.json"),
        br#"{"server_id":"original","url":"/api/v3/manage/unchanged"}"#,
    )
    .unwrap();
    let document = format!("{FIXTURE_CONFIG}[listener]\nmode = 'disabled'\n");
    let config =
        HostConfiguration::parse_toml(document.as_bytes(), base, Path::new("server.toml")).unwrap();
    std::fs::write(base.join("server.toml"), document).unwrap();
    Worker::open(
        config,
        Arc::new(vesta_state::SystemClock),
        None,
        host,
        server,
    )
    .unwrap()
}

#[test]
fn host_default_access_discovery_and_pack_bans() {
    let temp = tempfile::tempdir().unwrap();
    let base = temp.path();
    let state =
        HostState::open(base.join("host-state"), Arc::new(vesta_state::SystemClock)).unwrap();
    let host = state.snapshot().unwrap().host_id;
    let account = Uuid::new_v4();
    let key = Ed25519KeyPair::from_seed_unchecked(&[61; 32]).unwrap();
    let owner_device = state
        .claim(
            account,
            "Owner",
            key.public_key().as_ref().try_into().unwrap(),
        )
        .unwrap();
    let stranger_account = Uuid::new_v4();
    let stranger_key = Ed25519KeyPair::from_seed_unchecked(&[62; 32]).unwrap();
    let stranger = state
        .write(|c| {
            HostState::enroll_device_in(
                c,
                stranger_account,
                "Linked",
                stranger_key.public_key().as_ref().try_into().unwrap(),
                state.now_ms(),
            )
        })
        .unwrap();
    let server = Uuid::new_v4();
    state
        .register_server(
            Actor::Account(account),
            &ServerEntry {
                server_id: server,
                name: "Fixture".into(),
                game_port: 25565,
                state: ServerState::Stopped,
                visibility: Visibility::Private,
            },
            state.snapshot().unwrap().revision,
        )
        .unwrap();
    let identity = Arc::new(HostIdentity::new(state.clone()).unwrap());
    let router = HostRouter::new(
        identity,
        "Test machine".into(),
        Arc::new(FolderBrowser::new(vec![], vec![base.join("host-state")]).unwrap()),
    );
    let worker = fixture_worker(base, host, server);
    router.add_worker(Arc::new(worker.handle()));
    let transport = Transport::start_host(router, &fixture_listener(base)).unwrap();
    let mut wire = Wire::connect(transport.local_addr());
    let owner = format!(
        "Bearer {}",
        session(&mut wire, host, owner_device.device_id, &key)
    );
    let stranger_token = format!(
        "Bearer {}",
        session(&mut wire, host, stranger.device_id, &stranger_key)
    );
    let (_, info) = wire.request("GET", "/v1/host", Value::Null, Some(&owner));
    assert!(
        info["capabilities"]
            .as_array()
            .unwrap()
            .contains(&json!("public-access"))
    );
    let access_path = format!("/v1/servers/{server}/manage/default-access");
    let (status, access) = wire.request("GET", &access_path, Value::Null, Some(&owner));
    assert_eq!(status, 200);
    let starting = &access["data"];
    assert_eq!(starting["default_role_id"], Value::Null);
    assert_eq!(starting["visibility"], "private");
    assert_eq!(
        starting["eligible_roles"],
        json!([{"id":"pack-user","name":"Pack User"}])
    );
    assert_eq!(starting["can_manage_visibility"], true);
    let here = "/v1/here?port=25565";
    assert_eq!(wire.request("GET", here, Value::Null, None).0, 404);
    assert_eq!(
        wire.request("GET", "/v1/here?port=25565&port=25566", Value::Null, None)
            .0,
        400
    );
    let put = |role_id: Value, visibility, expected_version: &Value| json!({"default_role_id":role_id,"visibility":visibility,"expected_version":expected_version});
    let (status, error) = wire.request(
        "PUT",
        &access_path,
        put(json!("administrator"), "public", &starting["version"]),
        Some(&owner),
    );
    assert_eq!(
        (status, &error["error"]["code"]),
        (403, &json!("management_forbidden"))
    );
    assert_eq!(
        wire.request(
            "PUT",
            &access_path,
            put(Value::Null, "public", &starting["version"]),
            Some(&owner)
        )
        .0,
        400
    );
    assert_eq!(
        wire.request(
            "PUT",
            &access_path,
            json!({"visibility":"private","expected_version":starting["version"]}),
            Some(&owner)
        )
        .0,
        400
    );
    let (status, public) = wire.request(
        "PUT",
        &access_path,
        put(json!("pack-user"), "public", &starting["version"]),
        Some(&owner),
    );
    assert_eq!(status, 200);
    assert_eq!(public["data"]["visibility"], "public");
    assert_eq!(public["data"]["default_role_id"], "pack-user");
    assert_eq!(
        wire.request(
            "PUT",
            &access_path,
            put(Value::Null, "private", &starting["version"]),
            Some(&owner)
        )
        .0,
        412
    );
    let (status, discovery) = wire.request("GET", here, Value::Null, None);
    assert_eq!(status, 200);
    assert_eq!(
        discovery,
        json!({"hostId":host,"fingerprint":info["fingerprint"],"serverId":server,"gamePort":25565,"pack":null})
    );
    state.observe_game_port(server, 25566).unwrap();
    assert_eq!(wire.request("GET", here, Value::Null, None).0, 404);
    assert_eq!(
        wire.request("GET", "/v1/here?port=25566", Value::Null, None)
            .0,
        200
    );
    state.observe_game_port(server, 25565).unwrap();
    let (_, servers) = wire.request(
        "GET",
        "/v1/host/servers",
        Value::Null,
        Some(&stranger_token),
    );
    assert_eq!(servers["servers"][0]["role"], "pack-user");
    assert_eq!(
        wire.request("GET", &access_path, Value::Null, Some(&stranger_token))
            .0,
        403
    );
    drop(wire);
    let mut wire = Wire::connect(transport.local_addr());
    let status_path = format!("/v1/servers/{server}/pack/status?installed=0");
    let ticket_path = format!("/v1/servers/{server}/pack/ticket");
    assert_eq!(wire.request("GET", &status_path, Value::Null, None).0, 401);
    assert_eq!(
        wire.request("GET", &status_path, Value::Null, Some(&stranger_token))
            .0,
        200
    );
    assert_eq!(
        wire.request("POST", &ticket_path, Value::Null, Some(&stranger_token))
            .0,
        200
    );
    let bans_path = base.join("game/banned-players.json");
    std::fs::write(&bans_path, json!([{"uuid":stranger_account}]).to_string()).unwrap();
    assert_eq!(
        wire.request("POST", &ticket_path, Value::Null, Some(&stranger_token))
            .0,
        403
    );
    assert_eq!(
        wire.request("GET", &status_path, Value::Null, Some(&stranger_token))
            .0,
        401
    );
    std::fs::remove_file(bans_path).unwrap();
    drop(wire);
    let mut wire = Wire::connect(transport.local_addr());
    let (_, access) = wire.request("GET", &access_path, Value::Null, Some(&owner));
    let (status, cleared) = wire.request(
        "PUT",
        &access_path,
        put(Value::Null, "private", &access["data"]["version"]),
        Some(&owner),
    );
    assert_eq!(status, 200);
    assert_eq!(cleared["data"]["default_role_id"], Value::Null);
    assert_eq!(wire.request("GET", here, Value::Null, None).0, 404);
    assert_eq!(
        wire.request("GET", &status_path, Value::Null, Some(&stranger_token))
            .0,
        404
    );
}

#[test]
fn host_end_to_end_claim_sessions_and_registry_mapping() {
    let temp = tempfile::tempdir().unwrap();
    let base = temp.path();
    let state =
        HostState::open(base.join("host-state"), Arc::new(vesta_state::SystemClock)).unwrap();
    let host = state.snapshot().unwrap().host_id;
    let account = Uuid::new_v4();
    let key = Ed25519KeyPair::from_seed_unchecked(&[61; 32]).unwrap();
    let public: [u8; 32] = key.public_key().as_ref().try_into().unwrap();
    let identity = Arc::new(HostIdentity::new(state.clone()).unwrap());
    identity.test_account(account, "TestOwner");
    let issued = identity
        .setup_code(LocalPrivilege::from_privileged_local_channel())
        .unwrap();
    let code = issued["code"].as_str().unwrap();
    let listener = fixture_listener(base);
    let router = HostRouter::new(
        identity.clone(),
        "Test machine".into(),
        Arc::new(FolderBrowser::new(vec![], vec![base.join("host-state")]).unwrap()),
    );
    let transport = Transport::start_host(router.clone(), &listener).unwrap();
    let mut wire = Wire::connect(transport.local_addr());
    assert_eq!(
        wire.request("GET", "/v1/host/discovery", Value::Null, None)
            .1["claimState"],
        "unclaimed"
    );
    let (client, start) = Client::start(
        &SetupCode::parse(code).unwrap(),
        Purpose::Claim,
        None,
        host.to_string(),
        public,
    );
    let (status, started) = wire.request(
        "POST",
        "/v1/host/pake/start",
        serde_json::to_value(&start).unwrap(),
        None,
    );
    assert_eq!(status, 200, "{started}");
    let started: Started = serde_json::from_value(started).unwrap();
    let exporter = wire
        .0
        .get_mut()
        .conn
        .export_keying_material([0; 32], vesta_host_client::EXPORTER_LABEL, Some(b""))
        .unwrap();
    let (confirmation, finish) = client
        .finish(&started, &transport.tls_fingerprint().unwrap(), &exporter)
        .unwrap();
    let (status, finished) = wire.request(
        "POST",
        "/v1/host/pake/finish",
        serde_json::to_value(finish).unwrap(),
        None,
    );
    assert_eq!(status, 200, "{finished}");
    let token = confirmation
        .verify(serde_json::from_value::<Finished>(finished).unwrap())
        .unwrap();
    let pake = format!("Pake {token}");
    let device_key = encode(&public);
    let (status,challenge)=wire.request("POST","/v1/host/accounts/challenge",json!({"accountId":account,"name":"TestOwner","deviceKey":device_key,"purpose":"claim","binding":started.exchange_id}),Some(&pake));
    assert_eq!(status, 200, "{challenge}");
    let signature = signed(
        &key,
        "vesta.host.account.v1",
        json!({"hostId":host,"deviceKey":device_key,"challengeId":challenge["challengeId"],"serverHash":challenge["serverHash"],"purpose":"claim","binding":started.exchange_id}),
    );
    let (status, proof) = wire.request(
        "POST",
        "/v1/host/accounts/complete",
        json!({"challengeId":challenge["challengeId"],"signature":signature}),
        Some(&pake),
    );
    assert_eq!(status, 200, "{proof}");
    let signature = signed(
        &key,
        "vesta.host.claim.v1",
        json!({"hostId":host,"exchangeId":started.exchange_id,"accountProofId":proof["accountProofId"],"deviceKey":device_key,"deviceName":"Laptop"}),
    );
    let claim = json!({"exchangeId":started.exchange_id,"accountProofId":proof["accountProofId"],"deviceName":"Laptop","signature":signature});
    let (status, claimed) = wire.request("POST", "/v1/host/claim", claim.clone(), Some(&pake));
    assert_eq!(status, 200, "{claimed}");
    let device = serde_json::from_value(claimed["deviceId"].clone()).unwrap();
    // A lost successful claim reply is an idempotent receipt; a fresh PAKE claim
    // with the already-consumed setup code must be refused.
    let (_, second) = Client::start(
        &SetupCode::parse(&code).unwrap(),
        Purpose::Claim,
        None,
        host.to_string(),
        public,
    );
    assert_eq!(
        wire.request(
            "POST",
            "/v1/host/pake/start",
            serde_json::to_value(second).unwrap(),
            None
        )
        .0,
        401
    );
    let owner = session(&mut wire, host, device, &key);
    let owner = format!("Bearer {owner}");
    assert_eq!(
        wire.request("GET", "/v1/host/servers", Value::Null, Some(&owner))
            .1["servers"],
        json!([])
    );
    let stranger_key = Ed25519KeyPair::from_seed_unchecked(&[62; 32]).unwrap();
    let stranger_account = Uuid::new_v4();
    let stranger = state
        .write(|c| {
            HostState::enroll_device_in(
                c,
                stranger_account,
                "Stranger",
                stranger_key.public_key().as_ref().try_into().unwrap(),
                state.now_ms(),
            )
        })
        .unwrap();
    let stranger_token = session(&mut wire, host, stranger.device_id, &stranger_key);
    let stranger_token = format!("Bearer {stranger_token}");
    let server = Uuid::new_v4();
    state
        .register_server(
            Actor::Account(account),
            &ServerEntry {
                server_id: server,
                name: "Fixture".into(),
                game_port: 25565,
                state: ServerState::Stopped,
                visibility: Visibility::Private,
            },
            state.snapshot().unwrap().revision,
        )
        .unwrap();
    let worker = fixture_worker(base, host, server);
    router.add_worker(Arc::new(worker.handle()));
    let (status, list) = wire.request("GET", "/v1/host/servers", Value::Null, Some(&owner));
    assert_eq!(status, 200);
    assert_eq!(list["servers"][0]["serverId"], server.to_string());
    assert_eq!(
        wire.request(
            "GET",
            "/v1/host/servers",
            Value::Null,
            Some(&stranger_token)
        )
        .1["servers"],
        json!([])
    );
    let path = format!("/v1/servers/{server}/manage/server");
    let (status, reply) = wire.request("GET", &path, Value::Null, Some(&owner));
    assert_eq!(status, 200, "{reply}");
    let unknown = format!("/v1/servers/{}/manage/server", Uuid::new_v4());
    let a = wire.request("GET", &path, Value::Null, Some(&stranger_token));
    let b = wire.request("GET", &unknown, Value::Null, Some(&stranger_token));
    assert_eq!(
        (a.0, a.1["error"]["code"].clone()),
        (b.0, b.1["error"]["code"].clone())
    );
    assert_eq!(a.0, 404);
    assert_eq!(
        wire.request(
            "POST",
            "/v1/host/imports/plan",
            json!({}),
            Some(&stranger_token)
        )
        .0,
        403
    );
    assert_eq!(
        wire.request("POST", "/v1/host/imports/plan", json!({}), Some(&owner))
            .0,
        503 // this router has no import service configured
    );
    assert_eq!(wire.2, Some(2));

    let mut conditional = Wire::connect(transport.local_addr());
    let role_path = format!("/v1/servers/{server}/manage/roles");
    let owner_header = format!("Authorization: {owner}\r\n");
    let paths = [
        "/v1/host/manage/capabilities".to_owned(),
        "/v1/host/manage/people".to_owned(),
        format!("/v1/host/manage/people/{stranger_account}/memberships"),
        "/v1/host/manage/role-presets".to_owned(),
        role_path.clone(),
        format!("{role_path}/capabilities"),
    ];
    // A removal or enablement request may hold this lock through worker
    // shutdown. These reads use their own database authorization.
    let removal = router.removal_lock.lock().unwrap();
    for path in &paths {
        let (status, data, tag) = conditional.raw("GET", path, &owner_header, b"");
        assert_eq!(status, 200, "{path}");
        assert!(!data.is_empty(), "{path}");
        let tag = tag.unwrap();
        let check = format!("{owner_header}If-None-Match: \"other\", {tag}\r\n");
        assert_eq!(
            conditional.raw("GET", path, &check, b""),
            (304, Vec::new(), Some(tag.clone())),
            "{path}"
        );
        let unauthorized = format!("Authorization: {stranger_token}\r\nIf-None-Match: {tag}\r\n");
        let (status, _, tag) = conditional.raw("GET", path, &unauthorized, b"");
        assert!(matches!(status, 403 | 404), "{path}: {status}");
        assert!(tag.is_none(), "{path}");
    }
    drop(removal);
    let (_, _, previous) = conditional.raw("GET", &role_path, &owner_header, b"");
    state
        .set_server_role(
            Actor::Account(account),
            server,
            stranger_account,
            Some(ServerRole::PackUser),
            state.snapshot().unwrap().revision,
        )
        .unwrap();
    let check = format!(
        "{owner_header}If-None-Match: {}\r\n",
        previous.as_deref().unwrap()
    );
    let (status, _, changed) = conditional.raw("GET", &role_path, &check, b"");
    assert_eq!(status, 200);
    assert_ne!(changed, previous);
    for suffix in ["manage/files?path=config", "manage/content?view=summary"] {
        let path = format!("/v1/servers/{server}/{suffix}");
        let (status, _, tag) = conditional.raw("GET", &path, &owner_header, b"");
        assert_eq!(status, 200, "{suffix}");
        let tag = tag.unwrap();
        let check = format!("{owner_header}If-None-Match: {tag}\r\n");
        assert_eq!(
            conditional.raw("GET", &path, &check, b""),
            (304, Vec::new(), Some(tag)),
            "{suffix}"
        );
    }
    drop(conditional);
    drop(wire);
    let mut wire = Wire::connect(transport.local_addr());
    let (status, pack) = wire.request(
        "GET",
        &format!("/v1/servers/{server}/pack/status?installed=0"),
        Value::Null,
        Some(&owner),
    );
    assert_eq!(status, 200, "{pack}");
    assert_eq!(pack["published"], false);
    let (status, ticket) = wire.request(
        "POST",
        &format!("/v1/servers/{server}/pack/ticket"),
        Value::Null,
        Some(&owner),
    );
    assert_eq!(status, 200, "{ticket}");
    assert!(
        ticket["artifactUrl"]
            .as_str()
            .unwrap()
            .starts_with(&format!("/v1/servers/{server}/"))
    );
    let ticket_auth = format!("Bearer {}", ticket["ticket"].as_str().unwrap());
    assert_eq!(
        wire.request(
            "GET",
            ticket["artifactUrl"].as_str().unwrap(),
            Value::Null,
            Some(&ticket_auth)
        )
        .0,
        404
    ); // valid ticket, no published pack
    drop(wire);
    let mut wire = Wire::connect(transport.local_addr());
    assert_eq!(
        wire.request(
            "GET",
            ticket["artifactUrl"].as_str().unwrap(),
            Value::Null,
            Some(&ticket_auth)
        )
        .0,
        401
    ); // consumed
    assert_eq!(
        wire.request("GET", "/v1/host/realtime", Value::Null, Some(&owner))
            .0,
        501
    );
    drop(wire);
    let mut file_wire = Wire::connect(transport.local_addr());
    let file_path = format!("/v1/servers/{server}/manage/files/content?path=config/test.json");
    let (status, body) = file_wire.request("GET", &file_path, Value::Null, Some(&owner));
    assert_eq!(status, 200);
    assert_eq!(
        body,
        json!({"server_id":"original","url":"/api/v3/manage/unchanged"})
    );
    assert!(file_wire.1.is_some());
    let ranged = format!("{owner}\r\nRange: bytes=0-0");
    assert_eq!(
        file_wire
            .request("GET", &file_path, Value::Null, Some(&ranged))
            .0,
        206
    );
    let etag = file_wire.1.clone().expect("range validator");
    let conditional_range = format!("{owner}\r\nRange: bytes=0-0\r\nIf-Range: {etag}");
    assert_eq!(
        file_wire
            .request("GET", &file_path, Value::Null, Some(&conditional_range))
            .0,
        206
    );
    let status_path = format!("/v1/servers/{server}/manage/server/status");
    assert_eq!(
        file_wire
            .request("GET", &status_path, Value::Null, Some(&owner))
            .0,
        200
    );
    let tag = file_wire.1.clone().expect("status validator");
    let conditional = format!("{owner}\r\nIf-None-Match: {tag}");
    assert_eq!(
        file_wire
            .request("GET", &status_path, Value::Null, Some(&conditional))
            .0,
        304
    );
    drop(file_wire);
    let mut streams = Vec::new();
    for suffix in [
        "manage/console/stream",
        "manage/files/follow?path=config/test.json",
        "manage/metrics/stream",
        "realtime?installed=0",
    ]
    .into_iter()
    .chain(std::iter::repeat_n("manage/console/stream", 5))
    {
        let mut stream = Wire::connect(transport.local_addr());
        let upgrade = format!(
            "{owner}\r\nConnection: upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ=="
        );
        let (status, reply) = stream.request(
            "GET",
            &format!("/v1/servers/{server}/{suffix}"),
            Value::Null,
            Some(&upgrade),
        );
        if streams.len() == 8 {
            assert_eq!(status, 503, "{reply}");
            assert_eq!(stream.2, Some(2));
        } else {
            assert_eq!(status, 101, "{suffix}: {reply}");
        }
        streams.push(stream);
    }
    drop(streams);
    let mut wire = Wire::connect(transport.local_addr());
    assert_eq!(
        wire.request(
            "DELETE",
            &format!("/v1/host/devices/{}", stranger.device_id),
            Value::Null,
            Some(&owner)
        )
        .0,
        204
    );
    let (status, reply) = wire.request(
        "GET",
        "/v1/host/servers",
        Value::Null,
        Some(&stranger_token),
    );
    assert_eq!(status, 401, "{reply}");
    assert_eq!(reply["error"]["code"], "invalid_session");
    drop(wire);
    assert!(matches!(
        transport.shutdown(Duration::from_secs(5)).unwrap(),
        TransportShutdown::Complete
    ));
    assert!(matches!(
        worker.shutdown(Duration::from_secs(5)).unwrap(),
        vesta_control::worker::WorkerShutdown::Complete
    ));
}

/// A device that signs in with its Minecraft account over one TLS connection.
#[derive(Clone)]
struct SignIn<'a> {
    host: Uuid,
    fingerprint: String,
    account: Uuid,
    name: &'a str,
    key: &'a Ed25519KeyPair,
}
impl SignIn<'_> {
    fn device_key(&self) -> String {
        encode(self.key.public_key().as_ref())
    }
    fn challenge(&self, wire: &mut Wire, binding: &str) -> (u16, Value) {
        wire.request(
            "POST",
            "/v1/host/accounts/challenge",
            json!({"accountId":self.account,"name":self.name,"deviceKey":self.device_key(),"purpose":"signin","binding":binding}),
            None,
        )
    }
    fn complete(&self, wire: &mut Wire, c: &Value) -> (u16, Value) {
        let signature = signed(
            self.key,
            "vesta.host.account.v1",
            json!({"hostId":self.host,"deviceKey":self.device_key(),"challengeId":c["challengeId"],"serverHash":c["serverHash"],"purpose":"signin","binding":self.fingerprint}),
        );
        wire.request(
            "POST",
            "/v1/host/accounts/complete",
            json!({"challengeId":c["challengeId"],"signature":signature}),
            None,
        )
    }
    fn enroll_body(&self, proof: &Value, device_name: &str) -> Value {
        let signature = signed(
            self.key,
            "vesta.host.signin.v1",
            json!({"hostId":self.host,"fingerprint":self.fingerprint,"accountProofId":proof["accountProofId"],"deviceKey":self.device_key(),"deviceName":device_name}),
        );
        json!({"accountProofId":proof["accountProofId"],"deviceKey":self.device_key(),"deviceName":device_name,"signature":signature})
    }
    /// Challenge, launcher-side serverHash check, completion and enrollment.
    fn run(&self, wire: &mut Wire, device_name: &str) -> (u16, Value, Value) {
        let (status, c) = self.challenge(wire, &self.fingerprint);
        assert_eq!(status, 200, "{c}");
        let expected = vesta_host_client::requests::account_server_hash(
            &self.host.to_string(),
            &self.device_key(),
            "signin",
            &self.fingerprint,
            &self.account.to_string(),
            c["nonce"].as_str().unwrap(),
        );
        assert_eq!(c["serverHash"], expected);
        let (status, proof) = self.complete(wire, &c);
        assert_eq!(status, 200, "{proof}");
        let body = self.enroll_body(&proof, device_name);
        let (status, reply) = wire.request("POST", "/v1/host/sign-in", body.clone(), None);
        (status, reply, body)
    }
}
#[test]
fn host_account_sign_in_enrolls_new_device_without_code() {
    let temp = tempfile::tempdir().unwrap();
    let base = temp.path();
    let state =
        HostState::open(base.join("host-state"), Arc::new(vesta_state::SystemClock)).unwrap();
    let host = state.snapshot().unwrap().host_id;
    let owner = Uuid::new_v4();
    let desk = Ed25519KeyPair::from_seed_unchecked(&[71; 32]).unwrap();
    let desk_device = state
        .claim(
            owner,
            "Desk",
            desk.public_key().as_ref().try_into().unwrap(),
        )
        .unwrap();
    let identity = Arc::new(HostIdentity::new(state.clone()).unwrap());
    identity.test_account(owner, "TestOwner");
    std::fs::write(
        base.join("cert.pem"),
        include_bytes!("../../tests/fixtures/test-only-server.pem"),
    )
    .unwrap();
    std::fs::write(
        base.join("key.pem"),
        include_bytes!("../../tests/fixtures/test-only-server-key.pem"),
    )
    .unwrap();
    let listener = TlsListener::host(
        "127.0.0.1".parse().unwrap(),
        25500,
        base.join("cert.pem"),
        base.join("key.pem"),
    )
    .unwrap()
    .on_ephemeral_port();
    let router = HostRouter::new(
        identity.clone(),
        "Test machine".into(),
        Arc::new(FolderBrowser::new(vec![], vec![base.join("host-state")]).unwrap()),
    );
    let transport = Transport::start_host(router, &listener).unwrap();
    let fingerprint = transport.tls_fingerprint().unwrap();
    let laptop = Ed25519KeyPair::from_seed_unchecked(&[72; 32]).unwrap();
    let device = SignIn {
        host,
        fingerprint: fingerprint.clone(),
        account: owner,
        name: "TestOwner",
        key: &laptop,
    };

    let mut wire = Wire::connect(transport.local_addr());
    // A challenge bound to any other certificate is refused before Mojang.
    assert_eq!(device.challenge(&mut wire, &"0".repeat(64)).0, 401);
    // A challenge completed on another connection (a relay) is refused.
    let (_, c) = device.challenge(&mut wire, &fingerprint);
    let mut relay = Wire::connect(transport.local_addr());
    assert_eq!(device.complete(&mut relay, &c).0, 401);
    drop(relay);

    let (status, enrolled, body) = device.run(&mut wire, "Work laptop\u{0007}");
    assert_eq!(status, 200, "{enrolled}");
    assert_eq!(enrolled["state"], "enrolled");
    assert_eq!(enrolled["accountId"], owner.to_string());
    assert_eq!(enrolled["fingerprint"], fingerprint);
    assert_eq!(enrolled["deviceName"], "Work laptop");
    let laptop_id = enrolled["deviceId"].as_str().unwrap().to_owned();
    // Replaying the consumed proof is refused.
    assert_eq!(wire.request("POST", "/v1/host/sign-in", body, None).0, 401);
    let laptop_session = format!("Bearer {}", enrolled["session"].as_str().unwrap());
    assert_eq!(
        wire.request(
            "GET",
            "/v1/host/servers",
            Value::Null,
            Some(&laptop_session)
        )
        .0,
        200
    );
    let (status, devices) = wire.request(
        "GET",
        "/v1/host/devices",
        Value::Null,
        Some(&laptop_session),
    );
    assert_eq!(status, 200);
    let listed = devices["devices"]
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["deviceId"] == laptop_id.as_str())
        .unwrap()
        .clone();
    assert_eq!(listed["name"], "Work laptop");
    assert!(listed["createdAt"].is_string());
    let (status, owners) =
        wire.request("GET", "/v1/host/owners", Value::Null, Some(&laptop_session));
    assert_eq!(status, 200, "{owners}");
    assert_eq!(
        owners["owners"],
        json!([{"accountId":owner,"role":"primary_owner","name":"TestOwner"}])
    );
    drop(wire);

    // An account with no role on this machine: uniform refusal, no device.
    let stranger_account = Uuid::new_v4();
    identity.test_account(stranger_account, "Stranger");
    let stranger_key = Ed25519KeyPair::from_seed_unchecked(&[73; 32]).unwrap();
    let stranger = SignIn {
        account: stranger_account,
        name: "Stranger",
        key: &stranger_key,
        ..device.clone()
    };
    let mut wire = Wire::connect(transport.local_addr());
    let (status, refused, _) = stranger.run(&mut wire, "Phone");
    assert_eq!(
        (status, refused["error"]["code"].clone()),
        (403, json!("account_has_no_access"))
    );
    assert_eq!(state.devices(Actor::LocalRecovery).unwrap().len(), 2);
    drop(wire);

    // Owner settings: only owners read owners or rename; approval mode blocks sign-in.
    let stranger_device = state
        .write(|c| {
            HostState::enroll_device_in(
                c,
                stranger_account,
                "Stranger",
                stranger_key.public_key().as_ref().try_into().unwrap(),
                state.now_ms(),
            )
        })
        .unwrap();
    let mut wire = Wire::connect(transport.local_addr());
    let stranger_session = format!(
        "Bearer {}",
        session(&mut wire, host, stranger_device.device_id, &stranger_key)
    );
    let owner_session = format!(
        "Bearer {}",
        session(&mut wire, host, desk_device.device_id, &desk)
    );
    assert_eq!(
        wire.request(
            "GET",
            "/v1/host/owners",
            Value::Null,
            Some(&stranger_session)
        )
        .0,
        403
    );
    assert_eq!(
        wire.request(
            "PATCH",
            "/v1/host",
            json!({"name":"Mine"}),
            Some(&stranger_session)
        )
        .0,
        403
    );
    assert_eq!(
        wire.request(
            "PATCH",
            "/v1/host",
            json!({"name":"x".repeat(64)}),
            Some(&owner_session)
        )
        .0,
        400
    );
    let (status, renamed) = wire.request(
        "PATCH",
        "/v1/host",
        json!({"name":"Family server","newDeviceSignIn":"approval"}),
        Some(&owner_session),
    );
    assert_eq!(status, 200, "{renamed}");
    assert_eq!(renamed["name"], "Family server");
    assert_eq!(renamed["newDeviceSignIn"], "approval");
    let (_, info) = wire.request("GET", "/v1/host", Value::Null, Some(&owner_session));
    assert_eq!(
        (info["name"].clone(), info["newDeviceSignIn"].clone()),
        (json!("Family server"), json!("approval"))
    );
    assert_eq!(
        wire.request("GET", "/v1/host/discovery", Value::Null, None)
            .1["name"],
        "Family server"
    );
    drop(wire);
    identity.test_account(owner, "TestOwner");
    let tablet = Ed25519KeyPair::from_seed_unchecked(&[74; 32]).unwrap();
    let mut wire = Wire::connect(transport.local_addr());
    let (status, refused, _) = SignIn {
        key: &tablet,
        ..device.clone()
    }
    .run(&mut wire, "Tablet");
    assert_eq!(
        (status, refused["error"]["code"].clone()),
        (403, json!("device_approval_required"))
    );
    assert_eq!(state.devices(Actor::LocalRecovery).unwrap().len(), 3);

    // Revoking the auto-enrolled device ends its session.
    assert_eq!(
        wire.request(
            "DELETE",
            &format!("/v1/host/devices/{laptop_id}"),
            Value::Null,
            Some(&owner_session)
        )
        .0,
        204
    );
    assert_eq!(
        wire.request(
            "GET",
            "/v1/host/servers",
            Value::Null,
            Some(&laptop_session)
        )
        .0,
        401
    );
    // Anonymous sign-in challenges are rate limited per address.
    let mut limited = 0;
    for _ in 0..6 {
        if device.challenge(&mut wire, &fingerprint).0 == 429 {
            assert!((1..=60).contains(&wire.2.unwrap()));
            limited += 1;
        }
    }
    assert!(limited > 0);
    drop(wire);
    assert!(matches!(
        transport.shutdown(Duration::from_secs(5)).unwrap(),
        TransportShutdown::Complete
    ));
}

#[path = "import_tests.rs"]
mod imports;
