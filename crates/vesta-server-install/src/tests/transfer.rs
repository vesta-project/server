use super::super::transfer::{controlled, public_ip, validate_url, Job, Role, Source, Transport};
use super::*;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    time::Instant,
};

const META: &str = "https://fill.papermc.io/v3/projects/paper";
struct RawServer {
    origin: url::Url,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for RawServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}
async fn raw(responses: Vec<Vec<u8>>) -> RawServer {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        for response in responses {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut received = Vec::new();
            let mut b = [0; 1024];
            while !received.windows(4).any(|v| v == b"\r\n\r\n") {
                let n = socket.read(&mut b).await.unwrap();
                if n == 0 {
                    break;
                }
                received.extend_from_slice(&b[..n]);
                assert!(received.len() < 8192);
            }
            let _ = socket.write_all(&response).await;
            let _ = socket.shutdown().await;
        }
    });
    RawServer {
        origin: url::Url::parse(&format!("http://{}", addr)).unwrap(),
        task,
    }
}
fn wire(body: &[u8], headers: &str) -> Vec<u8> {
    let mut out = format!("HTTP/1.1 200 OK\r\nConnection: close\r\n{}\r\n", headers).into_bytes();
    out.extend_from_slice(body);
    out
}
fn transport(origin: url::Url) -> Transport {
    let mut h = Transport::new(TransportConfig::direct());
    h.fixture_origin = Some(origin);
    h
}
fn limits() -> Limits {
    Limits {
        metadata_bytes: 8,
        artifact_bytes: 128,
        total_bytes: 1024,
        idle_ms: 100,
        total_ms: 2000,
        ..Limits::default()
    }
}
async fn meta(
    h: &Transport,
    j: &mut Job,
    r: &dyn ProgressReporter,
) -> Result<super::super::transfer::Received> {
    h.receive(
        Source {
            url: META,
            software: Software::Paper,
        },
        Role::Metadata,
        None,
        j,
        r,
        None,
    )
    .await
}
#[test]
fn source_policy_rejects_escapes_credentials_encodings_and_other_origins() {
    for u in [
        "http://fill.papermc.io/v3/projects/paper",
        "https://fill.papermc.io:444/v3/projects/paper",
        "https://user:secret@fill.papermc.io/v3/projects/paper",
        "https://fill.papermc.io/v3/projects/paper?q=secret",
        "https://fill.papermc.io/v3/projects/paper#fragment",
        "https://fill.papermc.io/v3/projects/%2fpaper",
        "https://fill.papermc.io/v3/projects/%2e/paper",
        "https://fill.papermc.io/not-projects/paper",
        "https://fill.papermc.io.evil.invalid/v3/projects/paper",
        "https://127.0.0.1/v3/projects/paper",
        "https://fill.papermc.io/v3/projects/\\paper",
    ] {
        assert_eq!(
            validate_url(u, Software::Paper, Role::Metadata)
                .err()
                .unwrap(),
            InstallError::SourceRefused
        );
    }
    assert!(validate_url(META, Software::Paper, Role::Metadata).is_ok());
    assert_eq!(
        validate_url(META, Software::Fabric, Role::Metadata)
            .err()
            .unwrap(),
        InstallError::Unavailable
    );
}
#[test]
fn direct_dns_policy_excludes_special_addresses() {
    for s in [
        "127.0.0.1",
        "10.0.0.1",
        "172.16.0.1",
        "192.168.0.1",
        "169.254.0.1",
        "100.64.0.1",
        "0.1.2.3",
        "192.0.2.1",
        "192.0.0.1",
        "192.88.99.1",
        "198.18.0.1",
        "224.0.0.1",
        "255.255.255.255",
        "::1",
        "::ffff:8.8.8.8",
        "fc00::1",
        "fe80::1",
        "2001:db8::1",
        "2002::1",
        "3fff::1",
    ] {
        assert!(!public_ip(s.parse().unwrap()), "{s}");
    }
    for s in ["8.8.8.8", "1.1.1.1", "192.0.66.10", "2606:4700:4700::1111"] {
        assert!(public_ip(s.parse().unwrap()));
    }
}
#[tokio::test]
async fn identification_and_identity_encoding_have_no_ambient_credentials() {
    let f = Fixture::new(Software::Paper).await;
    f.plan().await;
    for r in f.server.received_requests().await.unwrap() {
        assert!(r
            .headers
            .get("user-agent")
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with("VestaServerInstall/"));
        assert!(r
            .headers
            .get("user-agent")
            .unwrap()
            .to_str()
            .unwrap()
            .contains("https://github.com/Vesta-Project/launcher"));
        assert_eq!(r.headers.get("accept-encoding").unwrap(), "identity");
        for name in ["cookie", "authorization", "proxy-authorization"] {
            assert!(!r.headers.contains_key(name));
        }
    }
}
#[tokio::test]
async fn forbidden_initial_and_redirect_urls_are_refused_before_target_request() {
    let f = Fixture::new(Software::Paper).await;
    let mut j = Job::new(f.installer.limits, 0);
    assert_eq!(
        f.installer
            .http
            .receive(
                Source {
                    url: "https://untrusted.invalid/stolen",
                    software: Software::Paper
                },
                Role::Metadata,
                None,
                &mut j,
                &SilentProgressReporter,
                None
            )
            .await
            .err()
            .unwrap(),
        InstallError::SourceRefused
    );
    assert!(f.server.received_requests().await.unwrap().is_empty());
    Mock::given(method("GET"))
        .and(path("/v3/projects/paper"))
        .respond_with(
            ResponseTemplate::new(302)
                .insert_header("Location", "http://127.0.0.1/stolen?secret=x"),
        )
        .with_priority(1)
        .mount(&f.server)
        .await;
    assert_eq!(
        meta(&f.installer.http, &mut j, &SilentProgressReporter)
            .await
            .err()
            .unwrap(),
        InstallError::SourceRefused
    );
    let requests = f.server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].url.path(), "/v3/projects/paper");
}
#[tokio::test]
async fn allowed_relative_redirects_are_finite() {
    let f = Fixture::new(Software::Paper).await;
    Mock::given(method("GET"))
        .and(path("/v3/projects/paper"))
        .respond_with(ResponseTemplate::new(302).insert_header("Location", "/v3/projects/other"))
        .with_priority(1)
        .mount(&f.server)
        .await;
    mount(
        &f.server,
        "/v3/projects/other",
        ResponseTemplate::new(302).insert_header("Location", "/v3/projects/paper"),
    )
    .await;
    let mut j = Job::new(f.installer.limits, 0);
    assert_eq!(
        meta(&f.installer.http, &mut j, &SilentProgressReporter)
            .await
            .err()
            .unwrap(),
        InstallError::SourceRefused
    );
    assert_eq!(f.server.received_requests().await.unwrap().len(), 4);
}
#[tokio::test]
async fn content_length_and_real_stream_limits_are_independent() {
    for headers in [
        "",
        "Content-Length: 9\r\n",
        "Transfer-Encoding: chunked\r\n",
    ] {
        let body = if headers.starts_with("Transfer") {
            b"9\r\n123456789\r\n0\r\n\r\n".as_slice()
        } else {
            b"123456789".as_slice()
        };
        let s = raw(vec![wire(body, headers)]).await;
        let h = transport(s.origin.clone());
        let mut j = Job::new(limits(), 0);
        assert_eq!(
            meta(&h, &mut j, &SilentProgressReporter)
                .await
                .err()
                .unwrap(),
            InstallError::LimitExceeded
        );
        assert_eq!(
            j.bytes,
            if headers.starts_with("Content-Length") {
                0
            } else {
                9
            }
        );
    }
    let s = raw(vec![wire(b"12345678", "")]).await;
    let mut j = Job::new(limits(), 0);
    let data = meta(
        &transport(s.origin.clone()),
        &mut j,
        &SilentProgressReporter,
    )
    .await
    .unwrap();
    assert_eq!(data.data, b"12345678");
    assert_eq!(data.size, 8);
}
#[tokio::test]
async fn retries_charge_failed_body_bytes_and_stop_at_finite_attempts() {
    let response = wire(b"abc", "Content-Length: 8\r\n");
    let s = raw(vec![response.clone(), response.clone(), response]).await;
    let mut j = Job::new(limits(), 0);
    assert_eq!(
        meta(
            &transport(s.origin.clone()),
            &mut j,
            &SilentProgressReporter
        )
        .await
        .err()
        .unwrap(),
        InstallError::TransportFailed
    );
    assert_eq!(j.bytes, 9);
    let response = wire(b"abc", "Content-Length: 8\r\n");
    let s = raw(vec![response.clone(), response.clone(), response]).await;
    let mut l = limits();
    l.total_bytes = 5;
    let mut j = Job::new(l, 0);
    assert_eq!(
        meta(
            &transport(s.origin.clone()),
            &mut j,
            &SilentProgressReporter
        )
        .await
        .err()
        .unwrap(),
        InstallError::LimitExceeded
    );
}
#[tokio::test]
async fn transient_statuses_retry_but_content_encoding_range_and_not_found_do_not() {
    for status in [429, 500, 503] {
        let f = Fixture::new(Software::Paper).await;
        Mock::given(method("GET"))
            .and(path("/v3/projects/paper"))
            .respond_with(ResponseTemplate::new(status))
            .with_priority(1)
            .mount(&f.server)
            .await;
        let mut j = Job::new(f.installer.limits, 0);
        assert_eq!(
            meta(&f.installer.http, &mut j, &SilentProgressReporter)
                .await
                .err()
                .unwrap(),
            InstallError::TransportFailed
        );
        assert_eq!(f.server.received_requests().await.unwrap().len(), 3);
    }
    for (header, value) in [
        ("Content-Encoding", "gzip"),
        ("Content-Encoding", "identity"),
        ("Content-Range", "bytes 0-4/5"),
    ] {
        let s = raw(vec![wire(b"abc", &format!("{}: {}\r\n", header, value))]).await;
        let mut j = Job::new(limits(), 0);
        assert_eq!(
            meta(
                &transport(s.origin.clone()),
                &mut j,
                &SilentProgressReporter
            )
            .await
            .err()
            .unwrap(),
            InstallError::SourceRefused
        );
        assert_eq!(j.bytes, 0);
    }
}
#[tokio::test]
async fn artifacts_require_pins_and_owned_disk_sink() {
    let f = Fixture::new(Software::Paper).await;
    let p = f.plan().await;
    let mut j = Job::new(f.installer.limits, 0);
    for pin in [None, p.artifact.as_ref()] {
        assert_eq!(
            f.installer
                .http
                .receive(
                    Source {
                        url: &p.artifact.as_ref().unwrap().url,
                        software: Software::Paper
                    },
                    Role::Artifact,
                    pin,
                    &mut j,
                    &SilentProgressReporter,
                    None
                )
                .await
                .err()
                .unwrap(),
            InstallError::InvalidInput
        );
    }
}
#[tokio::test]
async fn wrong_digest_empty_artifact_and_exact_size_are_not_retryable() {
    for body in [b"incorrect artifact".as_slice(), b"".as_slice()] {
        let f = Fixture::new(Software::Purpur).await;
        let p = f.plan().await;
        mount(
            &f.server,
            &f.artifact_path,
            ResponseTemplate::new(200).set_body_bytes(body),
        )
        .await;
        assert_eq!(
            f.installer
                .install(
                    &p,
                    Approval::review(&p, ACK).unwrap(),
                    &SilentProgressReporter
                )
                .await
                .unwrap_err(),
            InstallError::ArtifactChanged
        );
        assert_eq!(
            f.server
                .received_requests()
                .await
                .unwrap()
                .iter()
                .filter(|r| r.url.path() == f.artifact_path)
                .count(),
            1
        );
    }
    let f = Fixture::new(Software::Paper).await;
    let p = f.plan().await;
    mount(
        &f.server,
        &f.artifact_path,
        ResponseTemplate::new(200).set_body_bytes(b"short"),
    )
    .await;
    assert_eq!(
        f.installer
            .install(
                &p,
                Approval::review(&p, ACK).unwrap(),
                &SilentProgressReporter
            )
            .await
            .unwrap_err(),
        InstallError::ArtifactChanged
    );
}
#[tokio::test]
async fn cancellation_interrupts_response_wait_and_pause_counts_against_deadline() {
    let f = Fixture::new(Software::Paper).await;
    Mock::given(method("GET"))
        .and(path("/v3/projects/paper"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_bytes(BODY)
                .set_delay(Duration::from_secs(3)),
        )
        .with_priority(1)
        .mount(&f.server)
        .await;
    let reporter = Reporter::default();
    let mut j = Job::new(f.installer.limits, 0);
    let start = Instant::now();
    let cancel = async {
        tokio::time::sleep(Duration::from_millis(50)).await;
        reporter.cancel.store(true, Ordering::SeqCst);
    };
    let (result, _) = tokio::join!(meta(&f.installer.http, &mut j, &reporter), cancel);
    assert_eq!(result.err().unwrap(), InstallError::Cancelled);
    assert!(start.elapsed() < Duration::from_secs(1));
    let reporter = Reporter::default();
    reporter.pause.store(true, Ordering::SeqCst);
    let mut l = limits();
    l.total_ms = 70;
    assert_eq!(
        Job::new(l, 0).ready(&reporter).await.unwrap_err(),
        InstallError::Deadline
    );
}
#[tokio::test]
async fn total_deadline_interrupts_inflight_wait_and_retry_backoff() {
    let f = Fixture::new(Software::Paper).await;
    Mock::given(method("GET"))
        .and(path("/v3/projects/paper"))
        .respond_with(ResponseTemplate::new(503))
        .with_priority(1)
        .mount(&f.server)
        .await;
    let mut l = f.installer.limits;
    l.total_ms = 50;
    let mut j = Job::new(l, 0);
    assert_eq!(
        meta(&f.installer.http, &mut j, &SilentProgressReporter)
            .await
            .err()
            .unwrap(),
        InstallError::Deadline
    );
    assert_eq!(f.server.received_requests().await.unwrap().len(), 1);
    assert_eq!(
        controlled(
            std::future::pending::<Result<()>>(),
            Instant::now() + Duration::from_millis(30),
            &SilentProgressReporter
        )
        .await
        .unwrap_err(),
        InstallError::Deadline
    );
}
#[test]
fn purpur_slashless_catalogue_root_is_metadata_only_not_artifact_policy() {
    assert!(validate_url(
        "https://api.purpurmc.org/v2/purpur",
        Software::Purpur,
        Role::Metadata
    )
    .is_ok());
    for u in [
        "https://api.purpurmc.org/v2/purpur",
        "https://api.purpurmc.org/v2/purpur-evil/download",
    ] {
        assert_eq!(
            validate_url(u, Software::Purpur, Role::Artifact)
                .err()
                .unwrap(),
            InstallError::SourceRefused
        );
    }
}
#[tokio::test]
async fn artifact_pins_must_match_source_and_metadata_cannot_bypass_buffer_cap() {
    let f = Fixture::new(Software::Paper).await;
    let p = f.plan().await;
    let temp = tempfile::tempdir().unwrap();
    let mut file = tokio::fs::File::create(temp.path().join("owned.part"))
        .await
        .unwrap();
    let mut j = Job::new(f.installer.limits, 0);
    assert_eq!(
        f.installer
            .http
            .receive(
                Source {
                    url: &p.artifact.as_ref().unwrap().url,
                    software: Software::Paper
                },
                Role::Artifact,
                None,
                &mut j,
                &SilentProgressReporter,
                Some(&mut file)
            )
            .await
            .err()
            .unwrap(),
        InstallError::InvalidInput
    );
    assert_eq!(
        f.installer
            .http
            .receive(
                Source {
                    url: META,
                    software: Software::Paper
                },
                Role::Metadata,
                p.artifact.as_ref(),
                &mut j,
                &SilentProgressReporter,
                None
            )
            .await
            .err()
            .unwrap(),
        InstallError::InvalidInput
    );
    let mut changed = p.artifact.clone().unwrap();
    changed.url.push('x');
    assert_eq!(
        f.installer
            .http
            .receive(
                Source {
                    url: &p.artifact.as_ref().unwrap().url,
                    software: Software::Paper
                },
                Role::Artifact,
                Some(&changed),
                &mut j,
                &SilentProgressReporter,
                Some(&mut file)
            )
            .await
            .err()
            .unwrap(),
        InstallError::InvalidInput
    );
    assert_eq!(j.bytes, 0);
}
#[tokio::test]
async fn artifact_stream_without_length_has_exact_cap_and_never_buffers_in_memory() {
    let f = Fixture::new(Software::Purpur).await;
    let p = f.plan().await;
    for cap in [BODY.len() as u64, BODY.len() as u64 - 1] {
        let s = raw(vec![wire(BODY, "")]).await;
        let h = transport(s.origin.clone());
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("owned.part");
        let mut file = tokio::fs::File::create(&target).await.unwrap();
        let mut l = limits();
        l.artifact_bytes = cap;
        let mut job = Job::new(l, 0);
        let received = h
            .receive(
                Source {
                    url: &p.artifact.as_ref().unwrap().url,
                    software: Software::Purpur,
                },
                Role::Artifact,
                p.artifact.as_ref(),
                &mut job,
                &SilentProgressReporter,
                Some(&mut file),
            )
            .await;
        if cap == BODY.len() as u64 {
            let r = received.unwrap();
            assert!(r.data.is_empty());
            assert_eq!(r.size, cap);
            assert_eq!(r.observed_sha256, model::sha256(BODY));
            assert_eq!(std::fs::read(&target).unwrap(), BODY);
        } else {
            assert_eq!(received.err().unwrap(), InstallError::LimitExceeded);
            assert_eq!(std::fs::metadata(&target).unwrap().len(), 0);
        }
        assert_eq!(job.bytes, BODY.len() as u64);
    }
}
#[test]
fn cumulative_arithmetic_overflow_fails_closed() {
    let mut j = Job::new(Limits::default(), u64::MAX);
    assert_eq!(j.charge(1).unwrap_err(), InstallError::LimitExceeded);
}
