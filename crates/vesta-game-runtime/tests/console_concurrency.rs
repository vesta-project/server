//! Two actually overlapping capture invocations without assuming scheduler order.
use std::{
    io::{self, Cursor, Read},
    sync::mpsc,
    thread,
    time::Duration,
};
use vesta_game_runtime::console::{CaptureReport, ConsoleCapture, ConsoleStream};

struct GatedReader {
    entered: mpsc::Sender<ConsoleStream>,
    release: Option<mpsc::Receiver<()>>,
    stream: ConsoleStream,
    bytes: Cursor<Vec<u8>>,
}
impl Read for GatedReader {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if let Some(release) = self.release.take() {
            self.entered.send(self.stream).unwrap();
            release
                .recv_timeout(Duration::from_secs(8))
                .expect("explicit release before timeout");
        }
        self.bytes.read(buffer)
    }
}

#[test]
fn overlapping_nonempty_captures_keep_global_cursors_stream_pairs_and_local_reports() {
    let capture = ConsoleCapture::new();
    let (entered_tx, entered_rx) = mpsc::channel();
    let (out_release_tx, out_release_rx) = mpsc::channel();
    let (err_release_tx, err_release_rx) = mpsc::channel();
    let mut jobs = Vec::new();
    for (stream, prefix, release) in [
        (ConsoleStream::Stdout, "out", out_release_rx),
        (ConsoleStream::Stderr, "err", err_release_rx),
    ] {
        let handle = capture.clone();
        let entered = entered_tx.clone();
        jobs.push(thread::spawn(move || {
            let bytes = (0..64)
                .map(|index| format!("{prefix}-{index}\n"))
                .collect::<String>()
                .into_bytes();
            handle.capture_reader(
                stream,
                GatedReader {
                    entered,
                    release: Some(release),
                    stream,
                    bytes: Cursor::new(bytes),
                },
            )
        }));
    }
    drop(entered_tx);
    let first = entered_rx.recv_timeout(Duration::from_secs(3));
    let second = entered_rx.recv_timeout(Duration::from_secs(3));
    let out_release = out_release_tx.send(());
    let err_release = err_release_tx.send(());
    let outcomes: Vec<_> = jobs.into_iter().map(|job| job.join()).collect();
    assert_ne!(
        first.expect("first capture actually reading"),
        second.expect("second capture actually reading")
    );
    out_release.expect("stdout waited for release");
    err_release.expect("stderr waited for release");
    for outcome in outcomes {
        assert_eq!(
            outcome.unwrap().unwrap(),
            CaptureReport {
                lines: 64,
                truncated_lines: 0
            }
        );
    }
    let page = capture.snapshot(0, 200).unwrap();
    assert_eq!(page.lines.len(), 128);
    assert_eq!(page.next_cursor, 128);
    assert_eq!(page.latest_cursor, 128);
    let mut out = Vec::new();
    let mut err = Vec::new();
    for (index, line) in page.lines.iter().enumerate() {
        assert_eq!(line.cursor, index as u64 + 1);
        assert!(!line.truncated);
        match line.stream {
            ConsoleStream::Stdout => out.push(line.text.clone()),
            ConsoleStream::Stderr => err.push(line.text.clone()),
        }
    }
    assert_eq!(
        out,
        (0..64)
            .map(|index| format!("out-{index}"))
            .collect::<Vec<_>>()
    );
    assert_eq!(
        err,
        (0..64)
            .map(|index| format!("err-{index}"))
            .collect::<Vec<_>>()
    );
}
