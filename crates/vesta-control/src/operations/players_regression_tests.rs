//! Controlled query driver checks. Gates are owned inert native callbacks, not a
//! production test feature or injected permission provider.
use super::*;
use crate::operations::OwnerSession;
use vesta_game_runtime::console::{ConsoleStream, MAX_RETAINED_LINES};

struct ManualQuery {
    game: GameLifecycle,
    driver: QueryDriver,
    endpoint: Arc<QueryEndpoint>,
    generation: GenerationId,
    blocker: Option<GuardedInput<PlayerQueryError>>,
    release: Option<mpsc::Sender<()>>,
    cwd: PathBuf,
    _owner: OwnerSession,
}
impl ManualQuery {
    fn new(harness: &Harness, block_writer: bool) -> Self {
        let owner = harness.operations.open_owner().unwrap();
        let mut game = GameLifecycle::new(harness.launch());
        let generation = game.start(|_| Ok(())).unwrap();
        game.publish().unwrap();
        wait_for(|| harness.cwd.join("player-query-ready").exists());
        let (endpoint, driver) = QueryDriver::new(
            harness.operations.clone(),
            owner.stop_flag(),
            game.view.clone(),
            game.console.clone(),
        );
        let mut owned = Self {
            game,
            driver,
            endpoint: Arc::new(endpoint),
            generation,
            blocker: None,
            release: None,
            cwd: harness.cwd.clone(),
            _owner: owner,
        };
        if block_writer {
            let (entered_tx, entered_rx) = mpsc::channel();
            let (release_tx, release_rx) = mpsc::channel();
            let mut gate = Some((entered_tx, release_rx));
            owned.blocker = Some(
                owned
                    .game
                    .process
                    .queue_list_guarded(generation, move || {
                        let (entered, release) = gate.take().unwrap();
                        entered.send(()).unwrap();
                        release.recv_timeout(WAIT).unwrap();
                        Err(PlayerQueryError::Unavailable) // this gate never writes any command
                    })
                    .unwrap(),
            );
            owned.release = Some(release_tx);
            entered_rx.recv_timeout(WAIT).unwrap();
        }
        owned
    }
    fn admitted(&self) {
        wait_for(|| self.endpoint.mailbox.lock().unwrap().pending.is_some());
    }
    fn tick(&mut self) {
        self.game.observe().unwrap();
        self.game.publish().unwrap();
        assert_eq!(self.driver.advance(&mut self.game, true, false), None);
    }
    fn release_writer(&mut self) {
        self.release.take().unwrap().send(()).unwrap();
    }
    fn finish(&mut self) {
        let until = Instant::now() + WAIT;
        while self.driver.current.is_some() {
            assert!(Instant::now() < until);
            self.tick();
            thread::sleep(Duration::from_millis(5));
        }
        assert!(!self.endpoint.mailbox.lock().unwrap().busy);
        if let Some(mut blocker) = self.blocker.take() {
            let mut result = None;
            wait_for(|| {
                result = blocker.try_result();
                result.is_some()
            });
            assert!(matches!(
                result,
                Some(Err(GuardedError::Guard(PlayerQueryError::Unavailable)))
            ));
        }
    }
}
impl Drop for ManualQuery {
    fn drop(&mut self) {
        if let Some(release) = self.release.take() {
            let _ = release.send(());
        }
        let _ = fs::write(self.cwd.join("release-game"), b"release");
        // game/native workers drop first; the exact DB lease drops last.
    }
}
fn assert_denied(result: PlayerQueryResult<PlayerObservation>) {
    match result {
        Err(PlayerQueryError::Operation(error)) => {
            assert_eq!(error.code(), "forbidden");
            assert_eq!(error.reason(), Some(identity::DenyReason::ExplicitDeny));
        }
        other => panic!("expected canonical ConsoleRead denial: {other:?}"),
    }
}

#[test]
fn failed_caller_read_finalization_leaves_no_mailbox_request_or_native_write() {
    let harness = Harness::new(false);
    let mut owned = harness.runtime();
    let generation = running(&harness, &owned.runtime);
    FAIL_READ_FINALIZATION.with(|flag| flag.set(true));
    let error = owned
        .runtime
        .query_players(&harness.subject, generation, QUERY_WAIT)
        .unwrap_err();
    assert_eq!(error.code(), "state_transaction_ended");
    let mailbox = owned.runtime.player_queries().mailbox.lock().unwrap();
    assert!(!mailbox.busy);
    assert!(mailbox.pending.is_none());
    drop(mailbox);
    assert!(harness.wrote_nothing());
    owned
        .runtime
        .query_players(&harness.subject, generation, QUERY_WAIT)
        .unwrap();
    assert_eq!(harness.commands().as_deref(), Some("list\n"));
    assert_eq!(shutdown(&mut owned), ShutdownOutcome::Complete);
}

#[test]
fn failed_native_admission_finalization_retains_receipt_until_actual_ack() {
    let harness = Harness::new(false);
    let mut owned = ManualQuery::new(&harness, true);
    thread::scope(|scope| {
        let endpoint = owned.endpoint.clone();
        let subject = harness.subject.clone();
        let generation = owned.generation;
        let caller = scope.spawn(move || endpoint.query(&subject, generation, QUERY_WAIT));
        owned.admitted();
        FAIL_READ_FINALIZATION.with(|flag| flag.set(true));
        owned.tick();
        assert_eq!(
            caller.join().unwrap().unwrap_err().code(),
            "state_transaction_ended"
        );
        assert!(owned.driver.writer_pending());
        assert!(owned.endpoint.mailbox.lock().unwrap().busy);
        assert_eq!(harness.db.close().unwrap_err().code(), "state_runtime_busy");
        owned.tick();
        assert!(
            owned.driver.writer_pending(),
            "error delivery is not native ACK"
        );
        assert!(harness.wrote_nothing());
        owned.release_writer();
        owned.finish();
        assert!(
            harness.wrote_nothing(),
            "the cancelled queued guard must refuse its write"
        );
    });
}

#[test]
fn revoked_console_read_after_admission_refuses_the_delayed_native_write() {
    let harness = Harness::new(false);
    let mut owned = ManualQuery::new(&harness, true);
    thread::scope(|scope| {
        let endpoint = owned.endpoint.clone();
        let subject = harness.subject.clone();
        let generation = owned.generation;
        let caller = scope.spawn(move || endpoint.query(&subject, generation, QUERY_WAIT));
        owned.admitted();
        owned.tick();
        assert!(owned.driver.writer_pending());
        harness.deny(Capability::ConsoleRead);
        owned.release_writer();
        owned.finish();
        assert_denied(caller.join().unwrap());
        assert!(harness.wrote_nothing());
    });
}

#[test]
fn revoked_console_read_after_a_legitimate_write_refuses_the_observation() {
    let harness = Harness::new(false);
    let mut owned = ManualQuery::new(&harness, false);
    thread::scope(|scope| {
        let endpoint = owned.endpoint.clone();
        let subject = harness.subject.clone();
        let generation = owned.generation;
        let caller = scope.spawn(move || endpoint.query(&subject, generation, QUERY_WAIT));
        owned.admitted();
        owned.tick();
        wait_for(|| harness.commands().as_deref() == Some("list\n"));
        harness.deny(Capability::ConsoleRead);
        owned.finish();
        assert_denied(caller.join().unwrap());
        assert_eq!(harness.commands().as_deref(), Some("list\n"));
    });
}

#[test]
fn timeout_and_shutdown_do_not_release_a_pending_query_writer() {
    let harness = Harness::new(false);
    let mut owned = ManualQuery::new(&harness, true);
    thread::scope(|scope| {
        let endpoint = owned.endpoint.clone();
        let subject = harness.subject.clone();
        let generation = owned.generation;
        let caller =
            scope.spawn(move || endpoint.query(&subject, generation, Duration::from_millis(250)));
        owned.admitted();
        owned.tick();
        assert!(matches!(
            caller.join().unwrap(),
            Err(PlayerQueryError::TimedOut)
        ));
        owned.tick();
        assert!(owned.driver.writer_pending());
        assert!(owned.endpoint.mailbox.lock().unwrap().busy);
        owned.endpoint.stopping.store(true, Ordering::Release);
        owned.tick();
        assert!(owned.driver.writer_pending());
        assert_eq!(harness.db.close().unwrap_err().code(), "state_runtime_busy");
        assert!(harness.db.directory().acquire_runner_lock().is_err());
        assert!(harness.wrote_nothing());
        owned.release_writer();
        owned.finish();
        assert!(harness.wrote_nothing());
    });
}

#[test]
fn actual_console_eviction_is_a_gap_not_a_player_result() {
    let capture = ConsoleCapture::new();
    capture
        .capture_reader(
            ConsoleStream::Stdout,
            std::io::Cursor::new(
                format!(
                    "{}{LIST_LINE}\n",
                    "unrelated\n".repeat(MAX_RETAINED_LINES + 1)
                )
                .into_bytes(),
            ),
        )
        .unwrap();
    let mut cursor = 0;
    let mut lines = 0;
    let mut bytes = 0;
    assert!(capture.snapshot(0, 1).unwrap().gap);
    assert!(matches!(
        scan(
            &capture,
            0,
            &mut cursor,
            &mut lines,
            &mut bytes,
            GenerationId::parse_hex(&"66".repeat(16)).unwrap()
        ),
        Err(PlayerQueryError::ConsoleGap)
    ));
}

#[test]
fn line_and_byte_scan_budgets_are_inclusive_and_never_skip_a_page() {
    let generation = GenerationId::parse_hex(&"66".repeat(16)).unwrap();
    for (padding, count, expect_lines, expect_bytes) in [
        (
            "x".to_owned(),
            MAX_PLAYER_QUERY_LINES,
            MAX_PLAYER_QUERY_LINES,
            MAX_PLAYER_QUERY_LINES,
        ),
        (
            "x".repeat(1_024),
            MAX_PLAYER_QUERY_BYTES / 1_024,
            MAX_PLAYER_QUERY_BYTES / 1_024,
            MAX_PLAYER_QUERY_BYTES,
        ),
    ] {
        let capture = ConsoleCapture::new();
        capture
            .capture_reader(
                ConsoleStream::Stdout,
                std::io::Cursor::new(format!("{padding}\n").repeat(count).into_bytes()),
            )
            .unwrap();
        let mut cursor = 0;
        let mut lines = 0;
        let mut bytes = 0;
        while lines < count {
            let previous = cursor;
            assert!(
                scan(&capture, 0, &mut cursor, &mut lines, &mut bytes, generation)
                    .unwrap()
                    .is_none()
            );
            assert!(cursor > previous);
        }
        assert_eq!(lines, expect_lines);
        assert_eq!(bytes, expect_bytes);
        capture
            .capture_reader(
                ConsoleStream::Stdout,
                std::io::Cursor::new(format!("{LIST_LINE}\n").into_bytes()),
            )
            .unwrap();
        assert!(matches!(
            scan(&capture, 0, &mut cursor, &mut lines, &mut bytes, generation),
            Err(PlayerQueryError::ScanLimit)
        ));
    }
}
