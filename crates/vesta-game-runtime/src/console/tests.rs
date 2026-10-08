use super::*;
use std::{
    io::Cursor,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::Duration,
};

#[test]
fn undelimited_raw_framing_has_a_real_bound_and_marks_overflow() {
    let mut framer = Framer::new();
    for _ in 0..RAW_LINE_BYTES {
        framer.push(b'a');
    }
    assert_eq!(framer.bytes.len(), RAW_LINE_BYTES);
    assert!(!framer.truncated);
    for _ in 0..100_000 {
        framer.push(b'b');
    }
    assert_eq!(framer.bytes.len(), RAW_LINE_BYTES);
    assert!(framer.truncated);
    assert!(framer.bytes.iter().all(|&byte| byte == b'a'));
}

#[test]
fn cursor_exhaustion_never_wraps_evicts_or_partially_records_a_line() {
    let capture = ConsoleCapture::new();
    let mut line = vec![b'a'; MAX_LINE_BYTES];
    line.push(b'\n');
    capture
        .capture_reader(ConsoleStream::Stdout, Cursor::new(line.repeat(128)))
        .unwrap();
    {
        let mut inner = capture.inner.lock().unwrap();
        assert_eq!(inner.text_bytes, MAX_RETAINED_BYTES);
        inner.latest = u64::MAX - 1;
    }
    let report = capture
        .capture_reader(ConsoleStream::Stderr, Cursor::new(line.clone()))
        .unwrap();
    assert_eq!(report.lines, 1);
    let (before, bytes) = {
        let inner = capture.inner.lock().unwrap();
        assert_eq!(inner.latest, u64::MAX);
        assert_eq!(inner.lines.back().unwrap().cursor, u64::MAX);
        assert_eq!(inner.lines.front().unwrap().cursor, 2);
        assert_eq!(inner.text_bytes, MAX_RETAINED_BYTES);
        (inner.lines.clone(), inner.text_bytes)
    };
    assert_eq!(
        capture.capture_reader(ConsoleStream::Stderr, Cursor::new(line)),
        Err(ConsoleError::CursorExhausted),
    );
    let inner = capture.inner.lock().unwrap();
    assert_eq!(inner.latest, u64::MAX);
    assert_eq!(inner.text_bytes, bytes);
    assert_eq!(inner.lines, before);
}

#[test]
fn poisoned_storage_refuses_capture_and_snapshot_with_fixed_errors() {
    let capture = ConsoleCapture::new();
    let poisoner = capture.clone();
    assert!(
        thread::spawn(move || {
            let _held = poisoner.inner.lock().unwrap();
            panic!("controlled storage poison");
        })
        .join()
        .is_err()
    );
    assert_eq!(capture.snapshot(0, 1), Err(ConsoleError::Unavailable));
    assert_eq!(
        capture.capture_reader(ConsoleStream::Stdout, Cursor::new(b"not retained\n")),
        Err(ConsoleError::Unavailable),
    );
    assert_eq!(ConsoleError::Unavailable.code(), "console_unavailable");
    assert_eq!(
        ConsoleError::Unavailable.to_string(),
        "Console capture failed"
    );
}

#[test]
fn blocking_reader_does_not_hold_storage_lock_or_block_a_snapshot() {
    struct BlockingReader {
        entered: mpsc::Sender<()>,
        release: mpsc::Receiver<()>,
        returned: Arc<AtomicBool>,
    }
    impl Read for BlockingReader {
        fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
            self.entered
                .send(())
                .expect("notify actual reader admission");
            self.release
                .recv_timeout(Duration::from_secs(8))
                .expect("test must release the reader before its wait expires");
            self.returned.store(true, Ordering::SeqCst);
            Ok(0)
        }
    }
    let capture = ConsoleCapture::new();
    capture
        .capture_reader(ConsoleStream::Stdout, Cursor::new(b"before\n"))
        .unwrap();
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let returned = Arc::new(AtomicBool::new(false));
    let writer = capture.clone();
    let reader_returned = returned.clone();
    let reader = thread::spawn(move || {
        writer.capture_reader(
            ConsoleStream::Stderr,
            BlockingReader {
                entered: entered_tx,
                release: release_rx,
                returned: reader_returned,
            },
        )
    });
    let entered = entered_rx.recv_timeout(Duration::from_secs(3));
    let (observed_tx, observed_rx) = mpsc::channel();
    let viewer = capture.clone();
    let observer = thread::spawn(move || {
        let page = viewer.snapshot(0, 1);
        observed_tx
            .send(page)
            .expect("return actual snapshot outcome");
    });
    let observed = observed_rx.recv_timeout(Duration::from_secs(3));
    let returned_before_release = returned.load(Ordering::SeqCst);
    let released = release_tx.send(());
    let reader_outcome = reader.join();
    let observer_outcome = observer.join();
    entered.expect("reader entered its actual blocking call");
    released.expect("reader was still waiting when released");
    assert!(
        !returned_before_release,
        "snapshot must progress BEFORE reader return"
    );
    let page = observed
        .expect("snapshot progressed while reader remained blocked")
        .expect("snapshot succeeded, not merely an error message");
    assert_eq!(page.lines.len(), 1);
    assert_eq!(page.lines[0].text, "before");
    assert_eq!(
        reader_outcome.expect("reader did not panic"),
        Ok(CaptureReport {
            lines: 0,
            truncated_lines: 0
        })
    );
    observer_outcome.expect("observer did not panic");
}
