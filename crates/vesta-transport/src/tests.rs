use super::*;
use std::time::Instant;

#[test]
fn bounded_blocking_work_is_retained_after_request_cancellation_and_joined() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .max_blocking_threads(WORKERS)
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let jobs = Arc::new(BlockingJobs::new(tokio::runtime::Handle::current()));
        let (started_tx, started_rx) = mpsc::channel();
        let mut release = Vec::new();
        let mut requests = Vec::new();
        for _ in 0..WORKERS {
            let (tx, rx) = mpsc::channel();
            release.push(tx);
            let started = started_tx.clone();
            let pool = jobs.clone();
            requests.push(tokio::spawn(async move {
                pool.execute(move || {
                    started.send(()).unwrap();
                    rx.recv().unwrap();
                    response(StatusCode::OK, "{}", "application/json")
                })
                .await
            }));
        }
        let deadline = Instant::now() + Duration::from_secs(3);
        let mut started = 0;
        while started < WORKERS {
            while started_rx.try_recv().is_ok() {
                started += 1;
            }
            assert!(Instant::now() < deadline);
            tokio::task::yield_now().await;
        }
        let saturated = jobs.execute(|| panic!("saturated work must not run")).await;
        assert_eq!(saturated.status(), StatusCode::SERVICE_UNAVAILABLE);
        for request in requests {
            request.abort();
            let _ = request.await;
        }
        assert_eq!(jobs.handles.lock().unwrap().len(), WORKERS);
        assert!(
            jobs.handles
                .lock()
                .unwrap()
                .iter()
                .all(|job| !job.is_finished())
        );
        // HTTP cancellation did not discard the real blocking tasks. Join all of
        // them after allowing completion, before dropping the owning runtime.
        for tx in release {
            tx.send(()).unwrap();
        }
        jobs.reap(true).await;
        assert!(jobs.handles.lock().unwrap().is_empty());
    });
}

#[test]
fn shutdown_timeout_returns_the_still_owning_runtime_then_reobservation_joins() {
    let tls_temp = tempfile::tempdir().unwrap();
    std::fs::write(
        tls_temp.path().join("cert.pem"),
        include_bytes!("../tests/fixtures/test-only-server.pem"),
    )
    .unwrap();
    std::fs::write(
        tls_temp.path().join("key.pem"),
        include_bytes!("../tests/fixtures/test-only-server-key.pem"),
    )
    .unwrap();
    let config = vesta_control::config::HostConfiguration::parse_toml(
        b"schema-version = 1\n[paths]\nstate = 'state'\ngame = 'game'\nscan-roots = ['game']\nlaunch-definition = 'launch.toml'\n[pack]\nname = 'Fixture'\nversion = '1'\nrevision = 1\nminimum-revision = 1\nminecraft-version = '1.21.1'\n[pack-access]\nmode = 'minecraft'\n[listener]\nmode = 'tls'\nbind = '127.0.0.1'\nport = 25567\ncertificate = 'cert.pem'\nprivate-key = 'key.pem'\nmax-connections = 1\nmax-request-bytes = 1024\nrequest-timeout-seconds = 1\n",
        tls_temp.path(),
        std::path::Path::new("host.toml"),
    )
    .unwrap();
    let vesta_control::config::ListenerDescription::Tls(listener) = config.listener() else {
        panic!("TLS fixture");
    };
    let tls = Arc::new(tls::Material::new(listener).unwrap());
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let (release_tx, release_rx) = mpsc::channel();
    let (started_tx, started_rx) = mpsc::sync_channel(1);
    let (stop, _stopped) = watch::channel(false);
    let (done, complete) = mpsc::sync_channel(1);
    let thread = thread::spawn(move || {
        runtime.block_on(async {
            tokio::task::spawn_blocking(move || {
                started_tx.send(()).unwrap();
                release_rx.recv().unwrap();
            })
            .await
            .unwrap();
        });
        drop(runtime);
        done.send(()).unwrap();
    });
    started_rx.recv_timeout(Duration::from_secs(3)).unwrap();
    let owner = Transport {
        address: "127.0.0.1:1".parse().unwrap(),
        tls,
        stop,
        complete,
        thread: Some(thread),
    };
    let started = Instant::now();
    let owner = match owner.shutdown(Duration::from_millis(20)).unwrap() {
        TransportShutdown::TimedOut(owner) => owner,
        TransportShutdown::Complete => panic!("live blocking work must retain ownership"),
    };
    assert!(started.elapsed() < Duration::from_secs(1));
    assert!(!owner.thread.as_ref().unwrap().is_finished());
    release_tx.send(()).unwrap();
    assert!(matches!(
        owner.shutdown(Duration::from_secs(3)).unwrap(),
        TransportShutdown::Complete
    ));
}

#[test]
fn rate_limit_retry_after_rounds_up_and_never_advertises_immediate_retry() {
    for (duration, seconds) in [
        (Duration::ZERO, "1"),
        (Duration::from_nanos(1), "1"),
        (Duration::from_millis(500), "1"),
        (Duration::from_secs(1), "1"),
        (Duration::from_millis(1001), "2"),
        (Duration::from_secs(60), "60"),
        (Duration::MAX, "18446744073709551615"),
    ] {
        let reply = rate_limited("peer_rate_limited", duration);
        assert_eq!(reply.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(reply.headers()["retry-after"], seconds);
        assert_eq!(reply.headers()["cache-control"], "no-store");
        assert_eq!(reply.headers()["connection"], "close");
        assert!(
            !reply
                .headers()
                .keys()
                .any(|name| name.as_str().starts_with("access-control-"))
        );
    }
}

#[test]
fn fixed_error_headers_do_not_contain_cors() {
    let response = error(StatusCode::UNAUTHORIZED, "pack_session_invalid");
    assert_eq!(response.headers()["content-type"], "application/json");
    assert_eq!(response.headers()["cache-control"], "no-store");
    assert_eq!(response.headers()["connection"], "close");
    assert!(
        !response
            .headers()
            .keys()
            .any(|header| header.as_str().starts_with("access-control-"))
    );
}

#[test]
fn chunked_response_holds_download_permit_until_completion_or_cancel() {
    use http_body_util::BodyExt;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    runtime.block_on(async {
        let budget = Arc::new(Semaphore::new(1));
        let mut reply = response(
            StatusCode::OK,
            Bytes::from_owner(DownloadOwner {
                data: vec![0; BODY_CHUNK * 3],
                _permit: Arc::new(budget.clone().try_acquire_owned().unwrap().into()),
            }),
            "application/zip",
        );
        assert!(budget.clone().try_acquire_owned().is_err());
        assert_eq!(
            reply
                .body_mut()
                .frame()
                .await
                .unwrap()
                .unwrap()
                .into_data()
                .unwrap()
                .len(),
            BODY_CHUNK
        );
        assert!(budget.clone().try_acquire_owned().is_err());
        drop(reply); // cancelled transmission
        assert_eq!(budget.available_permits(), 1);
        let mut reply = response(
            StatusCode::OK,
            Bytes::from_owner(DownloadOwner {
                data: vec![0; BODY_CHUNK * 3],
                _permit: Arc::new(budget.clone().try_acquire_owned().unwrap().into()),
            }),
            "application/zip",
        );
        while reply.body_mut().frame().await.is_some() {}
        drop(reply);
        assert_eq!(budget.available_permits(), 1);
    });
}

struct DiskFixture {
    _temp: tempfile::TempDir,
    directory: vesta_state::artifacts::ArtifactDirectory,
    name: vesta_state::artifacts::ArtifactName,
    path: std::path::PathBuf,
    size: u64,
    digest: [u8; 32],
}
impl DiskFixture {
    fn new(chunks: usize) -> Self {
        use std::io::Write;
        let temp = tempfile::tempdir().unwrap();
        let state = vesta_state::StateDirectory::new(temp.path().join("state"), None).unwrap();
        state.prepare().unwrap();
        let directory = state.artifacts().unwrap();
        let name = vesta_state::artifacts::ArtifactName::new("test.zip").unwrap();
        let mut file = directory.create_new(&name).unwrap();
        let data = [b'x'; BODY_CHUNK];
        let mut hash = Sha256::new();
        for _ in 0..chunks {
            file.write_all(&data).unwrap();
            hash.update(data);
        }
        file.sync_all().unwrap();
        drop(file);
        Self {
            path: temp.path().join("state/artifacts/test.zip"),
            _temp: temp,
            directory,
            name,
            size: (chunks * BODY_CHUNK) as u64,
            digest: hash.finalize().into(),
        }
    }
    fn body(&self, budget: &Arc<Semaphore>) -> ReplyBody {
        ReplyBody::Artifact(Box::new(ArtifactBody {
            stream: Some(ArtifactStream {
                file: self.directory.open(&self.name).unwrap().unwrap(),
                hash: Sha256::new(),
                expected_hash: self.digest,
                size: self.size,
                remaining: self.size,
                permit: Arc::new(budget.clone().try_acquire_owned().unwrap().into()),
            }),
            pending: None,
            deadline: None,
        }))
    }
}
fn disk_runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}
#[test]
fn disk_stream_memory_is_chunk_bounded_independent_of_artifact_size() {
    use http_body_util::BodyExt;
    disk_runtime().block_on(async {
        for chunks in [9, 257] {
            let disk = DiskFixture::new(chunks);
            let budget = Arc::new(Semaphore::new(DOWNLOADS));
            let mut body = disk.body(&budget);
            let mut seen = 0u64;
            let mut hash = Sha256::new();
            let mut last = None;
            while let Some(frame) = body.frame().await {
                let data = frame.unwrap().into_data().unwrap();
                assert!(!data.is_empty() && data.len() <= BODY_CHUNK);
                hash.update(&data);
                seen += data.len() as u64;
                assert_eq!(budget.available_permits(), DOWNLOADS - 1);
                // The body has no read-ahead queue: at most one bounded read
                // allocation, plus chunks still owned by Hyper/the caller.
                if let ReplyBody::Artifact(body) = &body {
                    assert!(body.pending.is_none());
                }
                last = Some(data);
            }
            assert_eq!(seen, disk.size);
            assert_eq!(&hash.finalize()[..], &disk.digest);
            drop(body);
            assert_eq!(budget.available_permits(), DOWNLOADS - 1);
            drop(last);
            assert_eq!(budget.available_permits(), DOWNLOADS);
        }
    });
}
#[test]
fn disk_stream_drop_releases_permit_after_inflight_chunk_drops() {
    use http_body_util::BodyExt;
    disk_runtime().block_on(async {
        let disk = DiskFixture::new(10);
        let budget = Arc::new(Semaphore::new(DOWNLOADS));
        let mut body = disk.body(&budget);
        let data = body.frame().await.unwrap().unwrap().into_data().unwrap();
        drop(body);
        assert_eq!(budget.available_permits(), DOWNLOADS - 1);
        drop(data);
        assert_eq!(budget.available_permits(), DOWNLOADS);
    });
}
#[test]
fn disk_stream_truncation_growth_and_same_size_rewrite_abort_before_final_chunk() {
    use http_body_util::BodyExt;
    use std::io::{Seek, Write};
    disk_runtime().block_on(async {
        for change in ["truncate", "grow", "rewrite"] {
            let disk = DiskFixture::new(5);
            let budget = Arc::new(Semaphore::new(DOWNLOADS));
            let mut body = disk.body(&budget);
            let first = body.frame().await.unwrap().unwrap().into_data().unwrap();
            drop(first);
            let mut writer = std::fs::OpenOptions::new()
                .write(true)
                .open(&disk.path)
                .unwrap();
            match change {
                "truncate" => writer.set_len(BODY_CHUNK as u64).unwrap(),
                "grow" => writer.set_len(disk.size + 1).unwrap(),
                _ => {
                    writer
                        .seek(std::io::SeekFrom::Start((BODY_CHUNK * 2) as u64))
                        .unwrap();
                    writer.write_all(b"changed").unwrap();
                }
            }
            writer.sync_all().unwrap();
            drop(writer);
            let mut sent = BODY_CHUNK as u64;
            let mut aborted = false;
            while let Some(frame) = body.frame().await {
                match frame {
                    Ok(frame) => sent += frame.into_data().unwrap().len() as u64,
                    Err(_) => {
                        aborted = true;
                        break;
                    }
                }
            }
            assert!(aborted, "{change}");
            assert!(
                sent < disk.size,
                "must not complete old Content-Length: {change}"
            );
            drop(body);
            assert_eq!(budget.available_permits(), DOWNLOADS);
        }
    });
}

#[test]
fn realtime_blocking_pool_saturation_cannot_consume_http_workers() {
    let http_runtime = tokio::runtime::Builder::new_current_thread()
        .max_blocking_threads(WORKERS)
        .enable_all()
        .build()
        .unwrap();
    let realtime_runtime = tokio::runtime::Builder::new_current_thread()
        .max_blocking_threads(WORKERS)
        .build()
        .unwrap();
    http_runtime.block_on(async {
        let realtime = Arc::new(BlockingJobs::new(realtime_runtime.handle().clone()));
        let http = BlockingJobs::new(tokio::runtime::Handle::current());
        let (started_tx, started_rx) = mpsc::channel();
        let mut release = Vec::new();
        let mut requests = Vec::new();
        for _ in 0..WORKERS {
            let (tx, rx) = mpsc::channel();
            release.push(tx);
            let started = started_tx.clone();
            let jobs = realtime.clone();
            requests.push(tokio::spawn(async move {
                jobs.run(move || {
                    started.send(()).unwrap();
                    rx.recv().unwrap();
                })
                .await
                .unwrap();
            }));
        }
        let deadline = Instant::now() + Duration::from_secs(3);
        let mut started = 0;
        while started < WORKERS {
            while started_rx.try_recv().is_ok() {
                started += 1;
            }
            assert!(Instant::now() < deadline);
            tokio::task::yield_now().await;
        }
        assert!(
            realtime
                .run(|| panic!("realtime budget exceeded"))
                .await
                .is_err()
        );
        // This action must complete while every realtime worker remains blocked.
        let result = tokio::time::timeout(Duration::from_secs(1), http.run(|| 42)).await;
        for tx in release {
            tx.send(()).unwrap();
        }
        for request in requests {
            request.await.unwrap();
        }
        realtime.reap(true).await;
        http.reap(true).await;
        assert_eq!(result.unwrap().unwrap(), 42);
    });
}

#[test]
fn realtime_control_rate_has_bounded_burst_and_no_accumulated_idle_credit() {
    let now = Instant::now();
    let mut rate = realtime::ControlRate::new(now);
    for _ in 0..8 {
        assert!(rate.admit(now));
    }
    assert!(!rate.admit(now));
    assert!(rate.admit(now + Duration::from_secs(1)));
    assert!(!rate.admit(now + Duration::from_secs(1)));
    let later = now + Duration::from_secs(3600);
    for _ in 0..8 {
        assert!(rate.admit(later));
    }
    assert!(!rate.admit(later));
}

#[test]
fn downloads_are_capped_per_session_per_peer_and_globally_and_slots_are_cleaned() {
    let runtime = disk_runtime();
    let _guard = runtime.enter();
    let a: std::net::IpAddr = "192.0.2.1".parse().unwrap();
    let b: std::net::IpAddr = "192.0.2.2".parse().unwrap();
    let c: std::net::IpAddr = "192.0.2.3".parse().unwrap();
    let downloads = Downloads::<u32>::new(DOWNLOADS);
    let first = downloads.acquire(1, a).unwrap();
    // One session cannot hold a second slot, even from another address.
    assert!(downloads.acquire(1, b).is_none());
    // One address cannot hold a second slot, even with another session.
    assert!(downloads.acquire(2, a).is_none());
    // IPv4-mapped IPv6 spelling is the same peer.
    assert!(
        downloads
            .acquire(3, "::ffff:192.0.2.1".parse().unwrap())
            .is_none()
    );
    let second = downloads.acquire(2, b).unwrap();
    // Distinct sessions and peers still cannot exceed the global budget.
    assert!(downloads.acquire(3, c).is_none());
    assert_eq!(downloads.global.available_permits(), 0);
    let held = Arc::new(first);
    let chunk = held.clone();
    drop(held);
    // A chunk still owned by Hyper keeps the session and peer slots.
    assert!(downloads.acquire(1, c).is_none());
    drop(chunk);
    assert_eq!(downloads.global.available_permits(), 1);
    {
        let holders = downloads.holders.lock().unwrap();
        assert_eq!((holders.sessions.len(), holders.peers.len()), (1, 1));
    }
    let third = downloads.acquire(1, a).unwrap();
    drop((second, third));
    let holders = downloads.holders.lock().unwrap();
    assert!(holders.sessions.is_empty() && holders.peers.is_empty());
    assert_eq!(downloads.global.available_permits(), DOWNLOADS);
}

#[test]
fn connection_deadline_bounds_heads_but_streams_by_throughput_floor() {
    let limit = Duration::from_secs(2);
    let deadline = ConnectionDeadline::new(limit);
    let start = Instant::now();
    // Handshake, head and routing are bounded by request_timeout.
    assert!(deadline.check(start).is_some());
    assert!(
        deadline
            .check(start + limit + Duration::from_millis(1))
            .is_none()
    );
    // A body that keeps above the floor outlives request_timeout many times over.
    deadline.stream();
    let mut now = Instant::now();
    for _ in 0..10 {
        deadline.progress(deadline.floor() as usize);
        now += limit;
        assert!(deadline.check(now).is_some(), "steady download cut off");
    }
    // A window below the floor (a stall or trickle) closes the connection.
    deadline.progress(deadline.floor() as usize - 1);
    now += limit;
    assert!(deadline.check(now).is_none());
    // Body completion re-arms a bounded head deadline including the drain allowance.
    deadline.finish();
    let finished = Instant::now();
    assert!(deadline.check(finished + limit).is_some());
    assert!(
        deadline
            .check(finished + limit + DRAIN + Duration::from_millis(5))
            .is_none()
    );
}

#[test]
fn realtime_worker_saturation_closes_with_try_again_not_policy() {
    use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
    let http_runtime = disk_runtime();
    let realtime_runtime = tokio::runtime::Builder::new_current_thread()
        .max_blocking_threads(WORKERS)
        .build()
        .unwrap();
    http_runtime.block_on(async {
        let jobs = Arc::new(BlockingJobs::new(realtime_runtime.handle().clone()));
        let (stop_tx, stop) = watch::channel(false);
        let (started_tx, started_rx) = mpsc::channel();
        let mut release = Vec::new();
        let mut holders = Vec::new();
        for _ in 0..WORKERS {
            let (tx, rx) = mpsc::channel::<()>();
            release.push(tx);
            let started = started_tx.clone();
            let pool = jobs.clone();
            holders.push(tokio::spawn(async move {
                pool.run(move || {
                    started.send(()).unwrap();
                    rx.recv().unwrap();
                })
                .await
                .unwrap();
            }));
        }
        let deadline = Instant::now() + Duration::from_secs(3);
        let mut started = 0;
        while started < WORKERS {
            while started_rx.try_recv().is_ok() {
                started += 1;
            }
            assert!(Instant::now() < deadline);
            tokio::task::yield_now().await;
        }
        let busy = realtime::admitted(&jobs, &stop, Duration::from_millis(100), || {
            || -> Result<(), realtime::Refusal> { panic!("saturated pool must not run") }
        })
        .await;
        assert_eq!(busy, Err(realtime::Refusal::Busy));
        let code = realtime::Refusal::Busy.close_code();
        assert_eq!(code, CloseCode::Again);
        assert_eq!(u16::from(code), 1013);
        for tx in release {
            tx.send(()).unwrap();
        }
        for holder in holders {
            holder.await.unwrap();
        }
        jobs.reap(true).await;
        // Authority failures remain policy closes; free workers run the check.
        let revoked = realtime::admitted(&jobs, &stop, Duration::from_secs(1), || {
            || -> Result<(), realtime::Refusal> { Err(realtime::Refusal::Revoked) }
        })
        .await;
        assert_eq!(revoked, Err(realtime::Refusal::Revoked));
        assert_eq!(realtime::Refusal::Revoked.close_code(), CloseCode::Policy);
        assert_eq!(
            realtime::Refusal::Unavailable.close_code(),
            CloseCode::Error
        );
        stop_tx.send(true).unwrap();
        let stopping = realtime::admitted(&jobs, &stop, Duration::from_secs(1), || {
            || -> Result<(), realtime::Refusal> { Ok(()) }
        })
        .await;
        assert_eq!(stopping, Err(realtime::Refusal::Stopping));
        assert_eq!(realtime::Refusal::Stopping.close_code(), CloseCode::Away);
        jobs.reap(true).await;
    });
}

#[test]
fn tls_reload_reads_and_validates_without_holding_the_handshake_lock() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::write(
        temp.path().join("cert.pem"),
        include_bytes!("../tests/fixtures/test-only-server.pem"),
    )
    .unwrap();
    std::fs::write(
        temp.path().join("key.pem"),
        include_bytes!("../tests/fixtures/test-only-server-key.pem"),
    )
    .unwrap();
    let config = vesta_control::config::HostConfiguration::parse_toml(
        b"schema-version = 1\n[paths]\nstate = 'state'\ngame = 'game'\nscan-roots = ['game']\nlaunch-definition = 'launch.toml'\n[pack]\nname = 'Fixture'\nversion = '1'\nrevision = 1\nminimum-revision = 1\nminecraft-version = '1.21.1'\n[pack-access]\nmode = 'minecraft'\n[listener]\nmode = 'tls'\nbind = '127.0.0.1'\nport = 25567\ncertificate = 'cert.pem'\nprivate-key = 'key.pem'\nmax-connections = 1\nmax-request-bytes = 1024\nrequest-timeout-seconds = 1\n",
        temp.path(),
        std::path::Path::new("host.toml"),
    )
    .unwrap();
    let vesta_control::config::ListenerDescription::Tls(listener) = config.listener() else {
        panic!("TLS fixture");
    };
    let material = tls::Material::new(listener).unwrap();
    let before = material.fingerprint().unwrap();
    let mut observed = false;
    // The accept loop takes the state lock for every handshake. While the reload
    // reads and parses PEM files, that lock must be free.
    let changed = material
        .reload_with(|listener| {
            assert!(
                material.state.try_lock().is_ok(),
                "state locked during read"
            );
            assert!(material.config().is_ok());
            observed = true;
            tls::load(listener)
        })
        .unwrap();
    assert!(observed && !changed);
    // A failed read is still recorded and retains the previous identity.
    assert!(
        material
            .reload_with(|_| {
                assert!(material.state.try_lock().is_ok());
                Err(TransportError::TlsMaterial)
            })
            .is_err()
    );
    assert_eq!(material.last_error(), Some(TransportError::TlsMaterial));
    assert_eq!(material.fingerprint().unwrap(), before);
}
