use super::*;
use rand_chacha::ChaCha20Rng;
use std::sync::atomic::{AtomicBool, Ordering};
use vesta_host_client::rand_core::SeedableRng;
struct Authority(AtomicBool);
impl ClaimAuthority for Authority {
    fn is_unclaimed(&self) -> bool {
        !self.0.load(Ordering::SeqCst)
    }
    fn consume_claim(&self, _: &Capability) -> Result<()> {
        self.0
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .map(|_| ())
            .map_err(|_| PairingFailed)
    }
}
fn authority() -> Authority {
    Authority(AtomicBool::new(false))
}
const HOST: &str = "11111111-1111-4111-8111-111111111111";
const FP: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
fn code() -> SetupCode {
    SetupCode::parse("0123-4567-89AB").unwrap()
}
fn pair(
    state: &mut HostPake,
    a: &Authority,
    c: &Arc<Connection>,
    password: &SetupCode,
) -> Result<String> {
    let (client, start) = Client::start(password, Purpose::Claim, None, HOST.into(), [7; 32]);
    let started = state.start(a, c, "127.0.0.1".parse().unwrap(), start)?;
    let (client, finish) = client.finish(&started, FP, c.exporter())?;
    client.verify(state.finish(c, finish)?)
}
#[test]
fn rotation_consumption_and_scope() {
    let a = authority();
    let c = Connection::new([3; 32], FP.into());
    let mut s = HostPake::new(HOST.into());
    s.install_code(code(), Purpose::Claim, None, Instant::now())
        .unwrap();
    assert!(pair(&mut s, &a, &c, &SetupCode::parse("ZZZZ-ZZZZ-ZZZZ").unwrap()).is_err());
    assert!(s.claim.is_some());
    let token = pair(&mut s, &a, &c, &code()).unwrap();
    let key = encode(&[7; 32]);
    assert!(s.authorize(&c, &token, Purpose::Link, &key).is_err());
    assert!(
        s.authorize(&c, &token, Purpose::Claim, &encode(&[8; 32]))
            .is_err()
    );
    assert!(
        s.authorize(
            &Connection::new([3; 32], FP.into()),
            &token,
            Purpose::Claim,
            &key
        )
        .is_err()
    );
    s.rotate_setup(&a).unwrap();
    assert!(s.authorize(&c, &token, Purpose::Claim, &key).is_err());
    s.install_code(code(), Purpose::Claim, None, Instant::now())
        .unwrap();
    assert!(pair(&mut s, &a, &c, &SetupCode::parse("ZZZZ-ZZZZ-ZZZZ").unwrap()).is_err());
    assert!(s.claim.is_some());
    let token = pair(&mut s, &a, &c, &code()).unwrap();
    s.consume_claim(&a, &c, &token, &key).unwrap();
    assert!(s.consume_claim(&a, &c, &token, &key).is_err());
    assert!(s.rotate_setup(&a).is_err());
}
#[test]
fn bounds_expiry_disconnect_and_uniform_failure() {
    let a = authority();
    let c = Connection::new([3; 32], FP.into());
    let mut s = HostPake::new(HOST.into());
    s.install_code(code(), Purpose::Claim, None, Instant::now())
        .unwrap();
    for _ in 0..2 {
        let (_, r) = Client::start(&code(), Purpose::Claim, None, HOST.into(), [7; 32]);
        s.start(&a, &c, "127.0.0.1".parse().unwrap(), r).unwrap();
    }
    let (_, r) = Client::start(&code(), Purpose::Claim, None, HOST.into(), [7; 32]);
    assert_eq!(
        s.start(&a, &c, "127.0.0.1".parse().unwrap(), r).err(),
        Some(PairingFailed)
    );
    drop(c);
    s.reap(Instant::now());
    assert!(s.exchanges.is_empty());
    s.claim.as_mut().unwrap().deadline = Instant::now();
    s.reap(Instant::now());
    assert!(s.claim.is_none());
    assert_eq!(
        s.finish(
            &Connection::new([3; 32], FP.into()),
            Finish {
                exchange_id: "unknown".into(),
                client_confirm: encode(&[0; 32])
            }
        )
        .err(),
        Some(PairingFailed)
    );
}
#[test]
fn shared_vectors() {
    vesta_host_client_vector_check();
}
fn vesta_host_client_vector_check() {
    let fixture: serde_json::Value = serde_json::from_str(include_str!(
        "../../../protocol/host-v1/fixtures/pake-v1.json"
    ))
    .unwrap();
    let start: Start = serde_json::from_value(fixture["start"].clone()).unwrap();
    let started: Started = serde_json::from_value(fixture["started"].clone()).unwrap();
    let (a, b) = identities(&start);
    let (server, msg) = Spake2::<Ed25519Group>::start_b_with_rng(
        &Password::new(code().canonical().as_bytes()),
        &a,
        &b,
        ChaCha20Rng::from_seed([2; 32]),
    );
    assert_eq!(encode(&msg), started.message_b);
    let shared = server
        .finish(&decode::<33>(&start.message_a).unwrap())
        .unwrap();
    let confirmation = Confirmation::new(&shared, &[3; 32], &transcript(&start, &started).unwrap());
    assert_eq!(confirmation.client(), fixture["clientConfirm"]);
    assert_eq!(confirmation.server(), fixture["serverConfirm"]);
}
/// TCP loopback only. Application messages traverse rustls TLS 1.3 streams.
#[test]
fn real_tls13_positive_and_negatives() {
    use rustls_pki_types::{CertificateDer, PrivateKeyDer, ServerName, pem::PemObject};
    use std::{
        io::{BufRead, BufReader, Write},
        net::{TcpListener, TcpStream},
        thread,
    };
    for scenario in [
        "ok",
        "wrong_code",
        "exporter_mismatch",
        "replay",
        "expired_code",
        "reconnect",
    ] {
        let cert = CertificateDer::from_pem_slice(include_bytes!(
            "../tests/fixtures/test-only-server.pem"
        ))
        .unwrap();
        let key = PrivateKeyDer::from_pem_slice(include_bytes!(
            "../tests/fixtures/test-only-server-key.pem"
        ))
        .unwrap();
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let mut config = rustls::ServerConfig::builder_with_provider(provider.clone())
            .with_protocol_versions(&[&rustls::version::TLS13])
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(vec![cert.clone()], key)
            .unwrap();
        config.send_tls13_tickets = 0;
        config.session_storage = Arc::new(rustls::server::NoServerSessionStorage {});
        let mut roots = rustls::RootCertStore::empty();
        roots
            .add(
                CertificateDer::from_pem_slice(include_bytes!(
                    "../tests/fixtures/test-only-ca.pem"
                ))
                .unwrap(),
            )
            .unwrap();
        let client_config = rustls::ClientConfig::builder_with_provider(provider)
            .with_protocol_versions(&[&rustls::version::TLS13])
            .unwrap()
            .with_root_certificates(roots)
            .with_no_client_auth();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let task = thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(10)))
                .unwrap();
            let mut tls = rustls::ServerConnection::new(Arc::new(config)).unwrap();
            while tls.is_handshaking() {
                tls.complete_io(&mut socket).unwrap();
            }
            let exporter = tls
                .export_keying_material([0; 32], EXPORTER_LABEL, Some(b""))
                .unwrap();
            let context = Connection::from_tls(&tls, FP.into()).unwrap().unwrap();
            let a = authority();
            let mut state = HostPake::new(HOST.into());
            state
                .install_code(code(), Purpose::Claim, None, Instant::now())
                .unwrap();
            if scenario == "expired_code" {
                state.claim.as_mut().unwrap().deadline = Instant::now();
            }
            let mut stream = BufReader::new(rustls::StreamOwned::new(tls, socket));
            let mut line = String::new();
            stream.read_line(&mut line).unwrap();
            let started = state.start(
                &a,
                &context,
                "127.0.0.1".parse().unwrap(),
                serde_json::from_str(&line).unwrap(),
            );
            writeln!(
                stream.get_mut(),
                "{}",
                serde_json::to_string(&started.as_ref().ok()).unwrap()
            )
            .unwrap();
            stream.get_mut().flush().unwrap();
            if started.is_err() {
                return;
            }
            line.clear();
            stream.read_line(&mut line).unwrap();
            let finish: Finish = serde_json::from_str(&line).unwrap();
            let active = if scenario == "reconnect" {
                Connection::new(exporter, FP.into())
            } else {
                context
            };
            let result = state.finish(&active, finish.clone());
            if scenario == "replay" {
                assert_eq!(state.finish(&active, finish).err(), Some(PairingFailed));
            }
            writeln!(
                stream.get_mut(),
                "{}",
                serde_json::to_string(&result.ok()).unwrap()
            )
            .unwrap();
            stream.get_mut().flush().unwrap();
        });
        let mut socket = TcpStream::connect(addr).unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let mut tls = rustls::ClientConnection::new(
            Arc::new(client_config),
            ServerName::try_from("localhost").unwrap(),
        )
        .unwrap();
        while tls.is_handshaking() {
            tls.complete_io(&mut socket).unwrap();
        }
        assert_eq!(
            tls.protocol_version(),
            Some(rustls::ProtocolVersion::TLSv1_3)
        );
        let mut exporter = tls
            .export_keying_material([0; 32], EXPORTER_LABEL, Some(b""))
            .unwrap();
        if scenario == "exporter_mismatch" {
            exporter[0] ^= 1;
        }
        let password = if scenario == "wrong_code" {
            SetupCode::parse("ZZZZ-ZZZZ-ZZZZ").unwrap()
        } else {
            code()
        };
        let (client, start) = Client::start(&password, Purpose::Claim, None, HOST.into(), [7; 32]);
        let mut stream = BufReader::new(rustls::StreamOwned::new(tls, socket));
        writeln!(
            stream.get_mut(),
            "{}",
            serde_json::to_string(&start).unwrap()
        )
        .unwrap();
        stream.get_mut().flush().unwrap();
        let mut line = String::new();
        stream.read_line(&mut line).unwrap();
        let started: Option<Started> = serde_json::from_str(&line).unwrap();
        if scenario == "expired_code" {
            assert!(started.is_none());
            task.join().unwrap();
            continue;
        }
        let (client, finish) = client.finish(&started.unwrap(), FP, &exporter).unwrap();
        writeln!(
            stream.get_mut(),
            "{}",
            serde_json::to_string(&finish).unwrap()
        )
        .unwrap();
        stream.get_mut().flush().unwrap();
        line.clear();
        stream.read_line(&mut line).unwrap();
        let result: Option<Finished> = serde_json::from_str(&line).unwrap();
        if ["ok", "replay"].contains(&scenario) {
            client.verify(result.unwrap()).unwrap();
        } else {
            assert!(result.is_none(), "{scenario}");
        }
        task.join().unwrap();
    }
}

#[test]
fn rate_limits_exchange_and_capability_expiry() {
    let a = authority();
    let c = Connection::new([3; 32], FP.into());
    let mut s = HostPake::new(HOST.into());
    s.install_code(code(), Purpose::Claim, None, Instant::now())
        .unwrap();
    for _ in 0..5 {
        let (_, mut request) = Client::start(&code(), Purpose::Claim, None, HOST.into(), [7; 32]);
        request.host_id = "wrong".into();
        assert!(
            s.start(&a, &c, "127.0.0.1".parse().unwrap(), request)
                .is_err()
        );
    }
    let (_, request) = Client::start(&code(), Purpose::Claim, None, HOST.into(), [7; 32]);
    assert!(
        s.start(&a, &c, "127.0.0.1".parse().unwrap(), request)
            .is_err()
    );
    assert_eq!(s.starts, 5);
    s.window = Instant::now() - Duration::from_secs(61);
    s.reap(Instant::now());
    assert_eq!(s.starts, 0);
    for i in 1..=16 {
        let (_, request) = Client::start(&code(), Purpose::Claim, None, HOST.into(), [7; 32]);
        s.start(&a, &c, format!("127.0.0.{i}").parse().unwrap(), request)
            .unwrap();
    }
    let (_, request) = Client::start(&code(), Purpose::Claim, None, HOST.into(), [7; 32]);
    assert!(
        s.start(&a, &c, "127.0.0.17".parse().unwrap(), request)
            .is_err()
    );
    assert_eq!(s.exchanges.len(), 16);
    for exchange in s.exchanges.values_mut() {
        exchange.deadline = Instant::now();
    }
    s.reap(Instant::now());
    assert!(s.exchanges.is_empty());
    assert!(pair(&mut s, &a, &c, &SetupCode::parse("ZZZZ-ZZZZ-ZZZZ").unwrap()).is_err());
    assert!(s.claim.is_some());
    let token = pair(&mut s, &a, &c, &code()).unwrap();
    let key = encode(&[7; 32]);
    s.tokens
        .get_mut(&token_hash(&token).unwrap())
        .unwrap()
        .deadline = Instant::now();
    assert!(s.authorize(&c, &token, Purpose::Claim, &key).is_err());
    let mut s = HostPake::new(HOST.into());
    s.install_code(code(), Purpose::Claim, None, Instant::now())
        .unwrap();
    for i in 1..=5 {
        let (_, request) = Client::start(&code(), Purpose::Claim, None, HOST.into(), [7; 32]);
        let started = s
            .start(&a, &c, format!("127.0.0.{i}").parse().unwrap(), request)
            .unwrap();
        assert!(
            s.finish(
                &c,
                Finish {
                    exchange_id: started.exchange_id,
                    client_confirm: encode(&[0; 32])
                }
            )
            .is_err()
        );
    }
    assert!(s.backoff > Instant::now());
    assert!(s.claim.is_some());
    let (_, request) = Client::start(&code(), Purpose::Claim, None, HOST.into(), [7; 32]);
    assert!(
        s.start(&a, &c, "127.0.0.6".parse().unwrap(), request)
            .is_err()
    );
}

#[test]
fn link_code_uses_same_protocol_and_rotation_revokes_capability() {
    let a = authority();
    let c = Connection::new([3; 32], FP.into());
    let mut s = HostPake::new(HOST.into());
    let link = "22222222-2222-4222-8222-222222222222";
    let password = s.rotate_link(link.into()).unwrap();
    let (client, start) = Client::start(&password, Purpose::Link, None, HOST.into(), [7; 32]);
    let started = s
        .start(&a, &c, "127.0.0.1".parse().unwrap(), start)
        .unwrap();
    assert_eq!(started.link_id.as_deref(), Some(link));
    let (client, finish) = client.finish(&started, FP, c.exporter()).unwrap();
    let token = client.verify(s.finish(&c, finish).unwrap()).unwrap();
    let key = encode(&[7; 32]);
    let cap = s.authorize(&c, &token, Purpose::Link, &key).unwrap();
    assert!(s.authorize(&c, &token, Purpose::Claim, &key).is_err());
    s.consume_link(&cap).unwrap();
    assert!(s.authorize(&c, &token, Purpose::Link, &key).is_err());
    assert!(s.consume_link(&cap).is_err());
}
