use std::{
    fs,
    path::PathBuf,
    thread,
    time::{Duration, Instant},
};
use vesta_game_runtime::{
    console::{ConsoleCapture, ConsoleLine, ConsoleStream},
    process::{ExitSummary, GameProcess, GenerationId, GracefulStop, LaunchSpec, WaitOutcome},
};

pub const DEADLINE: Duration = Duration::from_secs(15);

// Fields drop in declaration order: the process and its workers precede the cwd.
pub struct Fixture {
    pub process: GameProcess,
    pub generation: GenerationId,
    pub console: ConsoleCapture,
    pub dir: tempfile::TempDir,
}
impl Fixture {
    pub fn new(args: Vec<String>, stop: GracefulStop) -> Self {
        let console = ConsoleCapture::new();
        let dir = tempfile::tempdir().unwrap();
        let mut process = GameProcess::new(console.clone());
        let spec = LaunchSpec::new(program(), args, dir.path().into(), stop).unwrap();
        let generation = process.start(spec).unwrap();
        let fixture = Self {
            process,
            generation,
            console,
            dir,
        };
        wait_line(&fixture.console, ConsoleStream::Stdout, "fixture-ready");
        wait_line(
            &fixture.console,
            ConsoleStream::Stderr,
            "fixture-stderr-ready",
        );
        assert_eq!(
            fs::read_to_string(fixture.dir.path().join("starts.txt")).unwrap(),
            "start\n"
        );
        let observed = fs::read_to_string(fixture.dir.path().join("cwd.txt")).unwrap();
        assert_eq!(
            PathBuf::from(observed).canonicalize().unwrap(),
            fixture.dir.path().canonicalize().unwrap()
        );
        fixture
    }
    pub fn recorded(&self, count: usize) -> Vec<String> {
        let deadline = Instant::now() + DEADLINE;
        loop {
            let lines: Vec<String> = fs::read_to_string(self.dir.path().join("commands.txt"))
                .unwrap_or_default()
                .lines()
                .map(str::to_owned)
                .collect();
            if lines.len() >= count {
                return lines;
            }
            assert!(
                Instant::now() < deadline,
                "fixture command delivery timed out"
            );
            thread::sleep(Duration::from_millis(5));
        }
    }
    pub fn stop(&mut self) -> ExitSummary {
        exited(self.process.stop(self.generation, DEADLINE).unwrap())
    }
}

pub fn program() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_vesta-game-fixture"))
}

pub fn exited(outcome: WaitOutcome) -> ExitSummary {
    match outcome {
        WaitOutcome::Exited(exit) => exit,
        WaitOutcome::TimedOut(snapshot) => panic!("fixture exit/drain timed out: {snapshot:?}"),
    }
}

pub fn all_lines(console: &ConsoleCapture) -> Vec<ConsoleLine> {
    let mut after = 0;
    let mut lines = Vec::new();
    loop {
        let page = console.snapshot(after, 200).unwrap();
        if page.lines.is_empty() {
            return lines;
        }
        assert!(page.next_cursor > after, "nonempty page must advance");
        after = page.next_cursor;
        lines.extend(page.lines);
    }
}

pub fn wait_line(console: &ConsoleCapture, stream: ConsoleStream, expected: &str) {
    let deadline = Instant::now() + DEADLINE;
    loop {
        if all_lines(console)
            .iter()
            .any(|line| line.stream == stream && line.text == expected)
        {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "fixture output handshake timed out"
        );
        thread::sleep(Duration::from_millis(5));
    }
}
