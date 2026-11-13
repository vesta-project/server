use super::*;
use ring::signature::{Ed25519KeyPair, KeyPair};
use rustls::{ClientConfig, ClientConnection, RootCertStore, StreamOwned};
use rustls_pki_types::{ServerName, pem::PemObject};
use std::{
    io::{BufRead, BufReader, Read, Write},
    net::TcpStream,
};
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
        Self(BufReader::new(stream), None)
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
            }
        }
        let mut body = vec![0; len];
        self.0.read_exact(&mut body).unwrap();
        (status, serde_json::from_slice(&body).unwrap_or(Value::Null))
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
    let document = "schema-version = 1\n[paths]\nstate = 'worker-state'\ngame = 'game'\nscan-roots = ['game/config']\nlaunch-definition = 'private/launch.toml'\n[pack]\nname = 'Worker'\nversion = '1.0.0'\nrevision = 1\nminimum-revision = 1\nminecraft-version = '1.21.1'\nunresolved-disposition = 'embed-public'\n[pack-access]\nmode = 'minecraft'\n[listener]\nmode = 'disabled'\n";
    let config = vesta_control::config::HostConfiguration::parse_toml(
        document.as_bytes(),
        base,
        std::path::Path::new("server.toml"),
    )
    .unwrap();
    std::fs::write(base.join("server.toml"), document).unwrap();
    let worker = Worker::open(
        config,
        Arc::new(vesta_state::SystemClock),
        None,
        host,
        server,
    )
    .unwrap();
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
    for suffix in [
        "manage/console/stream",
        "manage/files/follow?path=config/test.json",
        "manage/metrics/stream",
        "realtime?installed=0",
    ] {
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
        assert_eq!(status, 101, "{suffix}: {reply}");
        drop(stream);
    }
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
    assert_eq!(
        wire.request(
            "GET",
            "/v1/host/servers",
            Value::Null,
            Some(&stranger_token)
        )
        .0,
        401
    );
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

#[path = "import_tests.rs"]
mod imports;
