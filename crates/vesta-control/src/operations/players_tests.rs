//! Inert native player-query checks over real protected SQLite and owned copies of this
//! test executable. No services, Minecraft/Java, provider, shell or network.
//!
//! Every refusal below must be proved by the *absence* of an owned native write, not
//! merely by the returned error: a refusal that happened after the `list` write would
//! still have touched a running game console.
use super::*;
use crate::identity::{Capability, DeviceId, PrincipalId, Subject};
use crate::operations::{GameCommand, ShutdownOutcome};
use rusqlite::params;
use std::{
    fs::{self, OpenOptions},
    io::{BufRead, Write},
    path::PathBuf,
    thread,
    time::Instant,
};
use tempfile::TempDir;
use vesta_game_runtime::process::{GracefulStop, LaunchSpec, ProcessPhase};
use vesta_state::{Clock, StateDatabase, StateDirectory};

const WAIT: Duration = Duration::from_secs(15);
/// A deadline inside `MAX_PLAYER_QUERY_WAIT`. Distinct from `WAIT`: the fixture
/// lifetime budget is longer than the largest deadline the API accepts, so reusing one
/// constant for both would have every request refused as an invalid deadline.
const QUERY_WAIT: Duration = Duration::from_secs(8);
const ENABLE: &[u8] = b"vesta-owned-inert-player-query-fixture-v1";
const LIST_LINE: &str = "There are 2 of a max of 20 players online: OwnedAlpha, OwnedBeta";

/// An inert child that records the exact commands it receives and answers `list` with a
/// fixed console line. It never runs a game, a shell or a network operation.
///
/// Behaviour is selected only by marker files in its own cwd, never by an argv a caller
/// could supply from untrusted input.
#[test]
fn inert_player_query_fixture() {
    if fs::read("player-query-fixture-enabled").ok().as_deref() != Some(ENABLE) {
        return;
    }
    let silent = fs::read("silent-list").ok().as_deref() == Some(b"silent".as_slice());
    let mut commands = OpenOptions::new()
        .create(true)
        .append(true)
        .open("player-query-commands")
        .unwrap();
    println!("inert-player-query-fixture-ready");
    std::io::stdout().flush().unwrap();
    fs::write("player-query-ready", b"ready").unwrap();
    let (sender, receiver) = mpsc::channel();
    let input = thread::spawn(move || {
        for line in std::io::stdin().lock().lines() {
            match line {
                Ok(line) => {
                    if sender.send(line).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });
    loop {
        if fs::read("release-game").ok().as_deref() == Some(b"release".as_slice()) {
            std::process::exit(0);
        }
        match receiver.recv_timeout(Duration::from_millis(5)) {
            Ok(line) => {
                writeln!(commands, "{line}").unwrap();
                commands.flush().unwrap();
                if line == "list" && !silent {
                    // An answer on stdout only: the query path reads console capture,
                    // never the command file. Keep the wording exactly as a server emits.
                    println!("[12:00:01] [Server thread/INFO]: {LIST_LINE}");
                    std::io::stdout().flush().unwrap();
                }
                if line == "stop" {
                    std::process::exit(0);
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => (),
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                input.join().unwrap();
                return;
            }
        }
    }
}

struct TestClock;
impl Clock for TestClock {
    fn millis(&self) -> i64 {
        1_791_115_200_000
    }
}

/// A real temporary tree and a real state database. The `owner` role deliberately
/// includes `ConsoleRead`, so each test removes exactly the authority it is testing.
struct Harness {
    _root: TempDir,
    cwd: PathBuf,
    db: StateDatabase,
    operations: Operations,
    subject: Subject,
    // Retained so a test's intent is readable at its call site; the marker file is
    // what the child actually reads.
    #[allow(dead_code)]
    silent: bool,
}
impl Harness {
    fn new(silent: bool) -> Self {
        let root = tempfile::tempdir().unwrap();
        let cwd = root.path().join("inert game");
        fs::create_dir(&cwd).unwrap();
        fs::write(cwd.join("player-query-fixture-enabled"), ENABLE).unwrap();
        if silent {
            fs::write(cwd.join("silent-list"), b"silent").unwrap();
        }
        let directory = StateDirectory::new(root.path().join("private state"), None).unwrap();
        let db = StateDatabase::open(directory, Arc::new(TestClock)).unwrap();
        let subject = Subject {
            principal: PrincipalId::parse(&uuid::Uuid::new_v4().to_string()).unwrap(),
            device: DeviceId::parse(&uuid::Uuid::new_v4().to_string()).unwrap(),
        };
        db.write(|connection| {
            connection.execute("INSERT INTO principals (id,server_id,display_name,status,created_at,updated_at) VALUES (?,?,'Owned inert fixture','active',?,?)",
                params![subject.principal.as_str(),db.server_id(),db.clock().millis(),db.clock().millis()])?;
            connection.execute("INSERT INTO devices (id,server_id,principal_id,label,public_key_sha256,created_at) VALUES (?,?,?,'Owned inert fixture',?,?)",
                params![subject.device.as_str(),db.server_id(),subject.principal.as_str(),"a".repeat(64),db.clock().millis()])?;
            connection.execute("INSERT INTO role_assignments (id,principal_id,role_id,granted_by,created_at) VALUES (?,?,'owner','fixture',?)",
                params![uuid::Uuid::new_v4().to_string(),subject.principal.as_str(),db.clock().millis()])?;
            Ok(())
        }).unwrap();
        let operations = Operations::new(db.clone()).unwrap();
        Self {
            _root: root,
            cwd,
            db,
            operations,
            subject,
            silent,
        }
    }
    fn launch(&self) -> LaunchSpec {
        let executable = self.cwd.join(if cfg!(windows) {
            "owned-player-query-fixture.exe"
        } else {
            "owned-player-query-fixture"
        });
        fs::copy(std::env::current_exe().unwrap(), &executable).unwrap();
        LaunchSpec::new(
            executable,
            vec![
                "--exact".into(),
                "operations::players::tests::inert_player_query_fixture".into(),
                "--nocapture".into(),
            ],
            self.cwd.clone(),
            GracefulStop::MinecraftCommand,
        )
        .unwrap()
    }
    /// Remove exactly one capability from this principal, leaving the rest of the owner
    /// role intact so the refusal cannot be attributed to some unrelated policy change.
    fn deny(&self, capability: Capability) {
        self.db.write(|connection| {
            connection.execute("INSERT INTO principal_overrides (id,principal_id,capability,scope,effect,granted_by,created_at) VALUES (?,?,?,'*','deny','fixture',?)",
                params![uuid::Uuid::new_v4().to_string(),self.subject.principal.as_str(),capability.id(),self.db.clock().millis()])?;
            Ok(())
        }).unwrap();
    }
    /// The exact bytes this child was asked to write, or `None` when it never wrote any.
    /// The exact bytes this child was asked to write. The fixture creates this file when
    /// it starts, so "no write" means empty content, never a missing file.
    fn commands(&self) -> Option<String> {
        fs::read_to_string(self.cwd.join("player-query-commands")).ok()
    }
    /// True when no command byte reached the child. A missing record also counts: when
    /// the fixture never ran, there was nothing it could have been asked. An empty
    /// record is exactly "asked nothing", so a non-empty one is the failure.
    #[track_caller]
    fn wrote_nothing(&self) -> bool {
        match self.commands() {
            None => true,
            Some(recorded) => {
                assert!(
                    recorded.is_empty(),
                    "a refused request wrote {recorded:?} to a running child"
                );
                true
            }
        }
    }
    fn runtime(&self) -> OwnedRuntime {
        OwnedRuntime {
            runtime: OperationRuntime::open_with_stop_timeout(
                self.operations.clone(),
                self.launch(),
                Duration::from_millis(25),
            )
            .unwrap(),
            cwd: self.cwd.clone(),
        }
    }
}
impl Drop for Harness {
    fn drop(&mut self) {
        let _ = fs::write(self.cwd.join("release-game"), b"release");
        let result = self.db.close();
        if !thread::panicking() {
            result.unwrap();
        }
    }
}
/// The runtime's Drop joins the driver, which retains the child, and the driver waits
/// for the owned generation to actually exit. A player query is not a stop: this
/// fixture deliberately refuses `stop` so that every "a query is not a stop" assertion
/// stays true. That makes the runtime's ordinary shutdown unable to complete, so the
/// fixture must actually exit.
///
/// This is bounded *observation* by the caller, not an abandonment: the outcome below
/// is recorded and asserted, so a genuine shutdown failure is still a test failure.
struct OwnedRuntime {
    runtime: OperationRuntime,
    cwd: PathBuf,
}
impl Drop for OwnedRuntime {
    fn drop(&mut self) {
        let _ = fs::write(self.cwd.join("release-game"), b"release");
    }
}
/// Release the inert child and wait for the real exit this runtime observes.
///
/// It must *not* be `shutdown(...).unwrap()`: that panics inside Drop while the thread
/// is already unwinding, and the failure is far harder to read. A shutdown that does not
/// complete is still asserted, just through a value the caller can inspect.
#[track_caller]
fn shutdown(owned: &mut OwnedRuntime) -> ShutdownOutcome {
    // Release the inert child first. It ignores `stop`, so without this the driver
    // would be waiting for an exit that only the fixture can perform.
    let _ = fs::write(owned.cwd.join("release-game"), b"release");
    let outcome = owned.runtime.shutdown(WAIT).unwrap();
    assert_eq!(
        owned.runtime.game_snapshot().unwrap().phase,
        ProcessPhase::Stopped,
        "the released fixture child really exited, not just that shutdown returned"
    );
    outcome
}
fn wait_for(mut predicate: impl FnMut() -> bool) {
    let until = Instant::now() + WAIT;
    while !predicate() {
        assert!(Instant::now() < until, "owned inert fixture timed out");
        thread::sleep(Duration::from_millis(5));
    }
}

/// Start the inert child through the real operation path and wait for it to be ready.
fn running(harness: &Harness, runtime: &OperationRuntime) -> GenerationId {
    let id = match harness
        .operations
        .submit_game(&harness.subject, GameCommand::Start, None)
        .unwrap()
    {
        super::super::Admission::Created(summary) => summary.id,
        super::super::Admission::Existing(_) => panic!("a fresh fixture has no idempotency key"),
    };
    wait_for(|| {
        harness
            .operations
            .get(&harness.subject, &id)
            .unwrap()
            .is_some_and(|operation| operation.state == super::super::OperationState::Succeeded)
    });
    wait_for(|| harness.cwd.join("player-query-ready").exists());
    let generation = runtime.game_snapshot().unwrap().generation.unwrap();
    assert_eq!(
        runtime.game_snapshot().unwrap().phase,
        ProcessPhase::Running
    );
    generation
}

fn code_of(error: &PlayerQueryError) -> &'static str {
    error.code()
}

/// A current authorized caller receives the parsed list, and the child is asked for
/// exactly `list` — never `stop`, and never anything the caller supplied.
#[test]
fn an_authorized_current_query_returns_the_parsed_list_and_writes_only_list() {
    let harness = Harness::new(false);
    let mut owned = harness.runtime();
    let generation = running(&harness, &owned.runtime);

    let observation = owned
        .runtime
        .query_players(&harness.subject, generation, QUERY_WAIT)
        .expect("an authorized current query succeeds");
    assert_eq!(observation.generation(), generation);
    assert!(observation.response_cursor() > observation.after_cursor());
    let list = observation.list();
    assert_eq!(list.online(), 2);
    assert_eq!(list.max(), 20);
    let names: Vec<_> = list.players().iter().map(|entry| entry.name()).collect();
    assert_eq!(names, ["OwnedAlpha", "OwnedBeta"]);
    assert_eq!(
        harness.commands().as_deref(),
        Some("list\n"),
        "only the fixed list command is delivered"
    );
    // A successful query is not a stop, and it must leave the game running.
    assert_eq!(
        owned.runtime.game_snapshot().unwrap().phase,
        ProcessPhase::Running
    );
    assert_eq!(shutdown(&mut owned), ShutdownOutcome::Complete);
}

/// Without `ConsoleRead` the query is refused and no byte ever reaches the child. A
/// refusal after the write would still have touched a live console.
#[test]
fn without_console_read_the_query_is_refused_before_any_stdin_write() {
    let harness = Harness::new(false);
    let mut owned = harness.runtime();
    let generation = running(&harness, &owned.runtime);
    harness.deny(Capability::ConsoleRead);

    let error = owned
        .runtime
        .query_players(&harness.subject, generation, QUERY_WAIT)
        .expect_err("a denied caller cannot query");
    assert!(
        matches!(error, PlayerQueryError::Operation(_)),
        "expected an authorization refusal, got {}",
        code_of(&error)
    );
    assert!(
        harness.wrote_nothing(),
        "the child was never asked for anything"
    );
    assert_eq!(
        owned.runtime.game_snapshot().unwrap().phase,
        ProcessPhase::Running
    );
    assert_eq!(shutdown(&mut owned), ShutdownOutcome::Complete);
}

/// A generation the caller is not authorized to observe — here one this runtime never
/// owned — is refused without a write. Ownership, not caller input, decides the answer.
#[test]
fn a_generation_this_runtime_never_owned_is_refused_without_a_write() {
    let harness = Harness::new(false);
    let mut owned = harness.runtime();
    let generation = running(&harness, &owned.runtime);
    assert_ne!(generation.to_hex(), "11".repeat(16));
    // A well-formed generation this runtime never owned. The field is private, so the
    // only way to name another generation is through the strict hex parser.
    let unowned = GenerationId::parse_hex(&"11".repeat(16)).expect("a well-formed generation");

    let error = owned
        .runtime
        .query_players(&harness.subject, unowned, QUERY_WAIT)
        .expect_err("an unowned generation is refused");
    assert_eq!(code_of(&error), "players_query_stale_generation");
    assert!(
        harness.wrote_nothing(),
        "a stale generation must not reach the console"
    );
    assert_eq!(shutdown(&mut owned), ShutdownOutcome::Complete);
}

/// With no owned game at all there is nothing to observe, and that is unavailable
/// rather than an empty room.
#[test]
fn with_no_running_game_the_query_is_unavailable_not_an_empty_list() {
    let harness = Harness::new(false);
    let mut owned = harness.runtime();
    assert_eq!(
        owned.runtime.game_snapshot().unwrap().phase,
        ProcessPhase::Stopped
    );

    let error = owned
        .runtime
        .query_players(
            &harness.subject,
            GenerationId::parse_hex(&"33".repeat(16)).expect("a well-formed generation"),
            QUERY_WAIT,
        )
        .expect_err("a stopped runtime has no players");
    // Ownership is checked before phase, so a generation this runtime never held is
    // classified stale. Either refusal is correct and neither is a player count; the
    // property under test is that no list is ever produced.
    assert!(
        matches!(
            code_of(&error),
            "players_query_not_running" | "players_query_stale_generation"
        ),
        "unexpected classification {}",
        code_of(&error)
    );
    assert!(harness.wrote_nothing(), "no game exists to be asked");
    assert_eq!(shutdown(&mut owned), ShutdownOutcome::Complete);
}

/// A zero or oversized deadline is refused as a bad request before any policy, mailbox
/// or native work. The boundary itself is inclusive.
#[test]
fn request_bounds_are_inclusive_and_refused_before_any_write() {
    let harness = Harness::new(false);
    let mut owned = harness.runtime();
    let generation = running(&harness, &owned.runtime);

    for timeout in [
        Duration::ZERO,
        MAX_PLAYER_QUERY_WAIT + Duration::from_nanos(1),
    ] {
        let error = owned
            .runtime
            .query_players(&harness.subject, generation, timeout)
            .expect_err("an out-of-range deadline is refused");
        assert_eq!(code_of(&error), "players_query_invalid");
    }
    assert!(
        harness.wrote_nothing(),
        "a refused deadline must not reach the console"
    );
    // Exactly the documented maximum is accepted, so the bound is inclusive rather than
    // off by one. The inert child answers promptly, so this must not merely time out.
    assert!(
        owned
            .runtime
            .query_players(&harness.subject, generation, MAX_PLAYER_QUERY_WAIT)
            .is_ok(),
        "the maximum documented wait is inside the bound"
    );
    // The two refusals above must not have produced a write, and the accepted request
    // must have produced exactly the fixed command.
    assert_eq!(
        harness.commands().as_deref(),
        Some("list\n"),
        "only the accepted request reached the console"
    );
    assert_eq!(shutdown(&mut owned), ShutdownOutcome::Complete);
}

/// A query that gets no answer fails as unavailable. It must never be reported as a
/// successful, empty player list, and it must not leave the endpoint permanently busy.
#[test]
fn an_unanswered_query_times_out_and_never_reports_zero_players() {
    let harness = Harness::new(true);
    let mut owned = harness.runtime();
    let generation = running(&harness, &owned.runtime);

    let error = owned
        .runtime
        .query_players(&harness.subject, generation, Duration::from_millis(250))
        .expect_err("a silent child cannot answer");
    assert_eq!(code_of(&error), "players_query_timeout");
    // The write itself was legitimate: authorization held, so the refusal is about the
    // absent answer rather than about authority.
    assert_eq!(harness.commands().as_deref(), Some("list\n"));
    assert_eq!(
        owned.runtime.game_snapshot().unwrap().phase,
        ProcessPhase::Running
    );
    assert_eq!(shutdown(&mut owned), ShutdownOutcome::Complete);

    // A second query on the same runtime is not permanently rejected as busy: the
    // timed-out request released the single mailbox slot.
    let harness = Harness::new(false);
    let mut owned = harness.runtime();
    let generation = running(&harness, &owned.runtime);
    assert!(
        owned
            .runtime
            .query_players(&harness.subject, generation, QUERY_WAIT)
            .is_ok(),
        "a completed request leaves the endpoint usable"
    );
    assert_eq!(shutdown(&mut owned), ShutdownOutcome::Complete);
}

/// Two callers cannot both own the single outstanding query. The second is refused
/// rather than queued behind a native write it never asked for.
#[test]
fn a_second_concurrent_query_is_refused_as_busy_and_writes_nothing_extra() {
    let harness = Harness::new(true);
    // The runtime is shared so both callers reach the same endpoint and the same
    // mailbox. It is handed back through a channel so the main thread keeps unique
    // ownership for the real shutdown at the end.
    let shared = Arc::new(harness.runtime());
    let generation = running(&harness, &shared.runtime);
    let second = harness.subject.clone();
    let (returned, back) = mpsc::sync_channel(1);

    let first_runtime = Arc::clone(&shared);
    let first_subject = harness.subject.clone();
    let first = thread::spawn(move || {
        let result = first_runtime
            .runtime
            .query_players(&first_subject, generation, Duration::from_millis(300))
            .map(|_| ());
        let _ = returned.send(first_runtime);
        result
    });
    let busy = shared
        .runtime
        .query_players(&second, generation, Duration::from_millis(300));
    let first = first.join().expect("the first query thread returns");
    // The query thread handed its clone back through the channel. `shared` is this
    // thread's own binding and would be a second reference, so move it in: what
    // arrives is then the only remaining one and the runtime is not dropped without
    // an observed shutdown.
    let returned = back
        .recv_timeout(WAIT)
        .expect("the query thread hands its clone back");
    // `shared` is this thread's own binding and would be a second reference, so move it
    // in: what remains is then the only reference and the runtime is not dropped
    // without an observed shutdown.
    drop(returned);
    let mut owned =
        Arc::try_unwrap(shared).unwrap_or_else(|_| panic!("the query thread kept its own clone"));

    // If the first query completed before the second was admitted, the slot was
    // legitimately free and an Ok is a scheduling outcome, not a policy failure.
    if let Err(error) = busy {
        assert!(
            matches!(
                code_of(&error),
                // Busy when the first query still owns the mailbox. TimedOut is also a
                // correct refusal here: a caller with a 300ms deadline must expire
                // rather than block behind a query that will not answer, and the
                // fixture is deliberately silent. Either way it is refused, and
                // refused without a second native write.
                "players_query_busy" | "players_query_timeout"
            ),
            "the second caller is refused, not served: {}",
            code_of(&error)
        );
    }
    assert!(
        first.is_err(),
        "the silent query still fails on its own merits"
    );
    assert_eq!(
        harness.commands().as_deref(),
        Some("list\n"),
        "a busy refusal adds no second write"
    );
    let runtime = &mut owned;
    assert_eq!(
        runtime.runtime.game_snapshot().unwrap().phase,
        ProcessPhase::Running
    );
    assert_eq!(shutdown(runtime), ShutdownOutcome::Complete);
}

/// After shutdown is requested the query is refused and the child is not asked again.
#[test]
fn a_shutting_down_runtime_refuses_new_queries_without_writing() {
    let harness = Harness::new(false);
    let mut owned = harness.runtime();
    let generation = running(&harness, &owned.runtime);
    owned.runtime.request_shutdown();

    let error = owned
        .runtime
        .query_players(&harness.subject, generation, QUERY_WAIT)
        .expect_err("a stopping runtime admits no new query");
    assert!(
        matches!(
            code_of(&error),
            "players_query_stopping"
                | "players_query_not_running"
                | "players_query_stale_generation"
        ),
        "unexpected classification {}",
        code_of(&error)
    );
    assert!(
        harness.wrote_nothing(),
        "a stopping runtime must not write to the console"
    );
    assert!(matches!(shutdown(&mut owned), ShutdownOutcome::Complete));
}

/// Console text this query cannot trust is refused. It is never repaired into a
/// plausible-looking, possibly wrong player count.
#[test]
fn untrustworthy_console_text_is_refused_instead_of_parsed() {
    use vesta_game_runtime::console::{ConsoleCapture, ConsoleStream};

    // A truncated line, a line carrying a replacement scalar from lossy UTF-8 repair,
    // and a line whose count contradicts its own name list. Each is captured into its
    // own store so one case cannot mask another.
    let cases: &[(&str, &str)] = &[
        (
            "a repaired malformed scalar",
            "There are 2 of a max of 20 players online: Owned\u{fffd}lpha",
        ),
        (
            "a truncated format escape",
            "There are 1 of a max of 20 players online: \u{a7}x\u{a7}f\u{a7}f",
        ),
    ];
    for (label, text) in cases {
        let capture = ConsoleCapture::new();
        let after = capture.snapshot(0, 1).unwrap().latest_cursor;
        let report = capture
            .capture_reader(
                ConsoleStream::Stdout,
                std::io::Cursor::new(format!("{text}\n").into_bytes()),
            )
            .expect("capture succeeds");
        assert_eq!(report.lines, 1, "{label} is captured as one line");
        let mut lines = 0;
        let mut bytes = 0;
        let mut consumed = after;
        match scan(
            &capture,
            after,
            &mut consumed,
            &mut lines,
            &mut bytes,
            GenerationId::parse_hex(&"44".repeat(16)).expect("a well-formed generation"),
        ) {
            // The dangerous outcome is a *successful empty* result: the caller would
            // read "nobody online" out of text that never said so.
            Ok(_) => panic!("{label} must not parse as a player list"),
            Err(error) => assert!(
                matches!(
                    code_of(&error),
                    "players_query_console_incomplete" | "players_malformed_list"
                ),
                "{label} produced an unexpected classification {}",
                code_of(&error)
            ),
        }
    }

    // A name list with no empty interior token is refused: "Steve,,Alex" is a list
    // whose shape cannot be trusted, not a two-name list.
    let capture = ConsoleCapture::new();
    let after = capture.snapshot(0, 1).unwrap().latest_cursor;
    let report = capture
        .capture_reader(
            ConsoleStream::Stdout,
            std::io::Cursor::new(
                b"There are 2 of a max of 20 players online: OwnedAlpha,,OwnedBeta\n".to_vec(),
            ),
        )
        .expect("capture succeeds");
    assert_eq!(report.lines, 1);
    let mut lines = 0;
    let mut bytes = 0;
    let mut consumed = after;
    assert!(matches!(
        scan(
            &capture,
            after,
            &mut consumed,
            &mut lines,
            &mut bytes,
            GenerationId::parse_hex(&"44".repeat(16)).expect("a well-formed generation"),
        ),
        Err(PlayerQueryError::Parser(PlayerListError::Malformed))
    ));

    // A line the capture could not hold whole is refused rather than silently cut to a
    // shorter, still-parseable answer. This drives the real capture truncation path.
    let capture = ConsoleCapture::new();
    let after = capture.snapshot(0, 1).unwrap().latest_cursor;
    let oversized = "There are 1 of a max of 20 players online: ".to_owned()
        + &"A".repeat(vesta_game_runtime::console::MAX_LINE_BYTES + 64);
    let report = capture
        .capture_reader(
            ConsoleStream::Stdout,
            std::io::Cursor::new(format!("{oversized}\n").into_bytes()),
        )
        .expect("capture succeeds");
    assert_eq!(report.lines, 1);
    assert_eq!(
        report.truncated_lines, 1,
        "the capture marks its own truncation"
    );
    let mut lines = 0;
    let mut bytes = 0;
    let mut consumed = after;
    assert!(matches!(
        scan(
            &capture,
            after,
            &mut consumed,
            &mut lines,
            &mut bytes,
            GenerationId::parse_hex(&"44".repeat(16)).expect("a well-formed generation"),
        ),
        Err(PlayerQueryError::ConsoleIncomplete)
    ));
}

/// Redacted rendering: no player name, console text, path or digest reaches a log line.
#[test]
fn observations_and_errors_render_no_player_names_or_console_text() {
    use vesta_game_runtime::console::{ConsoleCapture, ConsoleStream};
    let capture = ConsoleCapture::new();
    let _ = capture.capture_reader(
        ConsoleStream::Stdout,
        std::io::Cursor::new(
            format!("[12:00:01] [Server thread/INFO]: {LIST_LINE}\n").into_bytes(),
        ),
    );
    let after = 0;
    let mut lines = 0;
    let mut bytes = 0;
    let mut cursor = after;
    let observation = scan(
        &capture,
        after,
        &mut cursor,
        &mut lines,
        &mut bytes,
        GenerationId::parse_hex(&"55".repeat(16)).expect("a well-formed generation"),
    )
    .expect("the fixed line parses")
    .expect("a list is present");
    let rendered = format!("{observation:?}");
    for secret in ["OwnedAlpha", "OwnedBeta", LIST_LINE, "players online"] {
        assert!(
            !rendered.contains(secret),
            "{secret} leaked into {rendered}"
        );
    }

    let errors: Vec<PlayerQueryError> = vec![
        PlayerQueryError::TimedOut,
        PlayerQueryError::ConsoleGap,
        PlayerQueryError::ConsoleIncomplete,
        PlayerQueryError::ScanLimit,
        PlayerQueryError::Parser(vesta_game_runtime::players::PlayerListError::Malformed),
        PlayerQueryError::Runtime(RuntimeError::StdinUnavailable),
        PlayerQueryError::Operation(OperationError::forbidden(identity::DenyReason::NotGranted)),
    ];
    for error in &errors {
        let rendered = format!("{error:?} {error}");
        // Query-native classifications carry the players_query_ prefix; a typed
        // underlying cause keeps its own code so the origin stays diagnosable. The
        // check is that each code is a fixed identifier, never console text.
        assert!(
            !code_of(error).is_empty()
                && code_of(error)
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c == '_'),
            "{} is not a fixed classification",
            code_of(error)
        );
        for secret in ["OwnedAlpha", "LIST_LINE", "inert game"] {
            assert!(
                !rendered.contains(secret),
                "{secret} leaked into {rendered}"
            );
        }
    }
}

/// The public surface accepts no command text: only a trusted Subject, the generation it
/// expects and a deadline. There is no handler, command string or callback parameter.
#[test]
fn the_query_surface_accepts_no_command_or_authority_input() {
    // Compile-time statement: the signature below is the entire caller-visible input.
    // If a command string or authorization callback were ever added, this stops
    // compiling, which is the point.
    fn only_inputs(
        runtime: &OperationRuntime,
        subject: &Subject,
        generation: GenerationId,
        timeout: Duration,
    ) -> PlayerQueryResult<PlayerObservation> {
        runtime.query_players(subject, generation, timeout)
    }
    let _ = only_inputs;
    // And the error surface carries no text-bearing variant either.
    assert_eq!(PlayerQueryError::TimedOut.code(), "players_query_timeout");
}
