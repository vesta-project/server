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
        let jobs = Arc::new(BlockingJobs {
            handles: Mutex::new(Vec::with_capacity(WORKERS)),
        });
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
                _permit: budget.clone().try_acquire_owned().unwrap(),
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
                _permit: budget.clone().try_acquire_owned().unwrap(),
            }),
            "application/zip",
        );
        while reply.body_mut().frame().await.is_some() {}
        drop(reply);
        assert_eq!(budget.available_permits(), 1);
    });
}
