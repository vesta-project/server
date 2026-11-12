//! Independent review-driven witnesses over inert owned readers, not game pipes.
use std::{
    collections::VecDeque,
    io::{self, Cursor, Read},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::Duration,
};
use vesta_game_runtime::console::{
    CaptureReport, ConsoleCapture, ConsoleError, ConsoleStream, MAX_LINE_BYTES, MAX_PAGE_LINES,
    MAX_RETAINED_BYTES,
};

struct ScriptedReader {
    steps: VecDeque<io::Result<Vec<u8>>>,
    dropped: Arc<AtomicBool>,
}
impl Read for ScriptedReader {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }
        match self.steps.pop_front() {
            None => Ok(0),
            Some(Err(error)) => Err(error),
            Some(Ok(bytes)) => {
                let count = bytes.len().min(buffer.len());
                buffer[..count].copy_from_slice(&bytes[..count]);
                if count < bytes.len() {
                    self.steps.push_front(Ok(bytes[count..].to_vec()));
                }
                Ok(count)
            }
        }
    }
}
impl Drop for ScriptedReader {
    fn drop(&mut self) {
        self.dropped.store(true, Ordering::SeqCst);
    }
}

#[test]
fn interruptions_preserve_crlf_and_partial_scalar_and_owned_readers_drop() {
    let dropped = Arc::new(AtomicBool::new(false));
    let capture = ConsoleCapture::new();
    let reader = ScriptedReader {
        steps: VecDeque::from([
            Ok(b"a\r".to_vec()),
            Err(io::ErrorKind::Interrupted.into()),
            Ok(b"\n\xe2".to_vec()),
            Err(io::ErrorKind::Interrupted.into()),
            Ok(b"\x82\xac\n".to_vec()),
        ]),
        dropped: dropped.clone(),
    };
    assert_eq!(
        capture
            .capture_reader(ConsoleStream::Stdout, reader)
            .unwrap(),
        CaptureReport {
            lines: 2,
            truncated_lines: 0
        }
    );
    assert!(dropped.load(Ordering::SeqCst));
    let page = capture.snapshot(0, 10).unwrap();
    assert_eq!(
        page.lines
            .iter()
            .map(|line| line.text.as_str())
            .collect::<Vec<_>>(),
        ["a", "€"]
    );

    dropped.store(false, Ordering::SeqCst);
    let reader = ScriptedReader {
        steps: VecDeque::from([
            Ok(b"kept\npartial-secret-4a2".to_vec()),
            Err(io::Error::other("native-secret-821")),
        ]),
        dropped: dropped.clone(),
    };
    let error = capture
        .capture_reader(ConsoleStream::Stderr, reader)
        .unwrap_err();
    assert_eq!(error, ConsoleError::ReadFailed);
    assert!(dropped.load(Ordering::SeqCst));
    let page = capture.snapshot(2, 10).unwrap();
    assert_eq!(page.lines.len(), 1);
    assert_eq!(page.lines[0].text, "kept");
    assert_eq!(page.lines[0].stream, ConsoleStream::Stderr);
    for diagnostic in [format!("{error:?}"), error.to_string()] {
        assert!(!diagnostic.contains("native-secret-821"));
        assert!(!diagnostic.contains("partial-secret-4a2"));
    }
}

#[test]
fn replacement_expansion_alone_can_truncate_below_the_raw_framing_bound() {
    for (count, truncated) in [(MAX_LINE_BYTES / 3, false), (MAX_LINE_BYTES / 3 + 1, true)] {
        let capture = ConsoleCapture::new();
        let report = capture
            .capture_reader(ConsoleStream::Stdout, Cursor::new(vec![0xff; count]))
            .unwrap();
        assert_eq!(report.lines, 1);
        assert_eq!(report.truncated_lines, u64::from(truncated));
        let page = capture.snapshot(0, 1).unwrap();
        assert_eq!(page.lines[0].text, "\u{fffd}".repeat(MAX_LINE_BYTES / 3));
        assert_eq!(page.lines[0].text.len(), (MAX_LINE_BYTES / 3) * 3);
        assert!(MAX_LINE_BYTES - page.lines[0].text.len() < 3);
        assert_eq!(page.lines[0].truncated, truncated);
    }
    let capture = ConsoleCapture::new();
    capture
        .capture_reader(ConsoleStream::Stdout, Cursor::new(b"x\xf0\x9f"))
        .unwrap();
    let page = capture.snapshot(0, 1).unwrap();
    assert_eq!(page.lines[0].text, "x\u{fffd}");
    assert!(!page.lines[0].truncated);
}

#[test]
fn mixed_size_publication_evicts_multiple_lines_and_keeps_exact_byte_budget() {
    let capture = ConsoleCapture::new();
    let mut input = vec![b'a'; 4_096];
    input.push(b'\n');
    let input = input.repeat(512);
    assert_eq!(
        capture
            .capture_reader(ConsoleStream::Stdout, Cursor::new(input))
            .unwrap()
            .lines,
        512
    );
    let mut large = vec![b'b'; MAX_LINE_BYTES];
    large.push(b'\n');
    capture
        .capture_reader(ConsoleStream::Stderr, Cursor::new(large))
        .unwrap();
    let first = capture.snapshot(0, 1).unwrap();
    // A full maximum-size line evicts exactly enough 4096-byte lines.
    let evicted = MAX_LINE_BYTES.div_ceil(4_096) as u64;
    assert_eq!(first.lines[0].cursor, evicted + 1);
    assert!(first.gap);
    assert!(!capture.snapshot(evicted, 1).unwrap().gap);
    assert!(capture.snapshot(evicted - 1, 1).unwrap().gap);
    let mut after = 0;
    let mut bytes = 0;
    let mut count = 0;
    loop {
        let page = capture.snapshot(after, MAX_PAGE_LINES).unwrap();
        if page.lines.is_empty() {
            break;
        }
        bytes += page.lines.iter().map(|line| line.text.len()).sum::<usize>();
        count += page.lines.len();
        assert!(page.next_cursor > after, "nonempty page must advance");
        after = page.next_cursor;
    }
    assert_eq!(count, 513 - evicted as usize);
    assert_eq!(bytes, MAX_RETAINED_BYTES);
    assert_eq!(after, 513);
    let last = capture.snapshot(512, 1).unwrap();
    assert_eq!(last.lines[0].text, "b".repeat(MAX_LINE_BYTES));
    assert_eq!(last.lines[0].stream, ConsoleStream::Stderr);
}

#[test]
fn diagnostics_redact_distinct_markers_but_snapshots_remain_raw_independent_copies() {
    let capture = ConsoleCapture::new();
    for (stream, marker) in [
        (ConsoleStream::Stdout, "stdout-secret-c492"),
        (ConsoleStream::Stderr, "stderr-secret-34bc"),
    ] {
        capture
            .capture_reader(stream, Cursor::new(format!("{marker}\n")))
            .unwrap();
    }
    let mut page = capture.snapshot(0, 2).unwrap();
    for diagnostic in [
        format!("{capture:?}"),
        format!("{page:?}"),
        format!("{:?}", page.lines[0]),
        format!("{:?}", page.lines[1]),
    ] {
        assert!(!diagnostic.contains("stdout-secret-c492"));
        assert!(!diagnostic.contains("stderr-secret-34bc"));
    }
    assert_eq!(page.lines[0].text, "stdout-secret-c492");
    assert_eq!(page.lines[1].text, "stderr-secret-34bc");
    page.lines[0].text.clear();
    assert_eq!(
        capture.snapshot(0, 1).unwrap().lines[0].text,
        "stdout-secret-c492"
    );
}

#[test]
fn another_stream_publishes_before_a_blocked_reader_returns() {
    struct BlockingReader {
        entered: mpsc::Sender<()>,
        release: mpsc::Receiver<()>,
        returned: Arc<AtomicBool>,
    }
    impl Read for BlockingReader {
        fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
            self.entered.send(()).unwrap();
            self.release
                .recv_timeout(Duration::from_secs(8))
                .expect("explicit test release before timeout");
            self.returned.store(true, Ordering::SeqCst);
            Ok(0)
        }
    }
    let capture = ConsoleCapture::new();
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let returned = Arc::new(AtomicBool::new(false));
    let blocked_capture = capture.clone();
    let blocked_returned = returned.clone();
    let blocked = thread::spawn(move || {
        blocked_capture.capture_reader(
            ConsoleStream::Stdout,
            BlockingReader {
                entered: entered_tx,
                release: release_rx,
                returned: blocked_returned,
            },
        )
    });
    let entered = entered_rx.recv_timeout(Duration::from_secs(3));
    let (published_tx, published_rx) = mpsc::channel();
    let publisher_capture = capture.clone();
    let publisher = thread::spawn(move || {
        let outcome =
            publisher_capture.capture_reader(ConsoleStream::Stderr, Cursor::new(b"other-stream\n"));
        published_tx.send(outcome).unwrap();
    });
    let published = published_rx.recv_timeout(Duration::from_secs(3));
    let returned_before_release = returned.load(Ordering::SeqCst);
    let release = release_tx.send(());
    let blocked_outcome = blocked.join();
    let publisher_outcome = publisher.join();
    entered.expect("reader entered");
    release.expect("reader still waited for explicit release");
    assert!(!returned_before_release);
    assert_eq!(
        published
            .expect("publication progressed before release")
            .unwrap(),
        CaptureReport {
            lines: 1,
            truncated_lines: 0
        }
    );
    assert_eq!(
        blocked_outcome.unwrap().unwrap(),
        CaptureReport {
            lines: 0,
            truncated_lines: 0
        }
    );
    publisher_outcome.unwrap();
    let page = capture.snapshot(0, 1).unwrap();
    assert_eq!(page.lines[0].cursor, 1);
    assert_eq!(page.lines[0].text, "other-stream");
    assert_eq!(page.lines[0].stream, ConsoleStream::Stderr);
}
