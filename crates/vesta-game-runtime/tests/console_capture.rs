//! Bounded public console-capture behavior over the real component and inert readers.
//!
//! Every assertion drives `vesta_game_runtime::console::ConsoleCapture` through its
//! published API only. No process, pipe, socket, thread, viewer callback or filesystem
//! is involved: readers are `Cursor`s and small custom `Read` values with bounded input,
//! so every count in this file is exact rather than inferred after the fact.
use std::io::{self, Cursor, Read};
use std::time::SystemTime;
use vesta_game_runtime::console::{
    CaptureReport, ConsoleCapture, ConsoleError, ConsoleLine, ConsolePage, ConsoleStream,
    MAX_LINE_BYTES, MAX_PAGE_BYTES, MAX_PAGE_LINES, MAX_RETAINED_BYTES, MAX_RETAINED_LINES,
};

/// The component's private read chunk. Pinned here only to keep byte accounting
/// below exact; no public constant promises this value.
const READ_BYTES: usize = 8_192;

/// Serves one fixed buffer repeatedly, so a capture drains an exact number of lines
/// without materializing the whole retained history up front.
struct RepeatingReader {
    chunk: Vec<u8>,
    remaining: usize,
    offset: usize,
}
impl RepeatingReader {
    fn new(chunk: Vec<u8>, copies: usize) -> Self {
        assert!(
            !chunk.is_empty(),
            "a repeating reader needs a non-empty chunk"
        );
        Self {
            chunk,
            remaining: copies,
            offset: 0,
        }
    }
}
impl Read for RepeatingReader {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if self.remaining == 0 || buffer.is_empty() {
            return Ok(0);
        }
        let mut copied = 0;
        while copied < buffer.len() && self.remaining > 0 {
            let piece = (self.chunk.len() - self.offset).min(buffer.len() - copied);
            buffer[copied..copied + piece]
                .copy_from_slice(&self.chunk[self.offset..self.offset + piece]);
            copied += piece;
            self.offset += piece;
            if self.offset == self.chunk.len() {
                self.remaining -= 1;
                self.offset = 0;
            }
        }
        Ok(copied)
    }
}

/// Limits each read to force delimiter and UTF-8 boundaries between calls.
struct ChunkedReader {
    data: Cursor<Vec<u8>>,
    max_chunk: usize,
}
impl ChunkedReader {
    fn new(data: Vec<u8>, max_chunk: usize) -> Self {
        assert!(max_chunk > 0);
        Self {
            data: Cursor::new(data),
            max_chunk,
        }
    }
}
impl Read for ChunkedReader {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let bound = buffer.len().min(self.max_chunk);
        self.data.read(&mut buffer[..bound])
    }
}

#[test]
fn repeating_reader_preserves_partial_chunks_and_exact_eof() {
    for buffer_size in [1, 2, 4, 7, 20] {
        let mut reader = RepeatingReader::new(b"abc\ndef".to_vec(), 3);
        let mut buffer = vec![0; buffer_size];
        let mut actual = Vec::new();
        loop {
            let read = reader.read(&mut buffer).unwrap();
            if read == 0 {
                break;
            }
            actual.extend_from_slice(&buffer[..read]);
        }
        assert_eq!(actual, b"abc\ndef".repeat(3));
        assert_eq!(reader.read(&mut buffer).unwrap(), 0);
    }
}

/// The stored texts of a page, borrowed so no assertion needs a temporary String.
fn line_texts(page: &ConsolePage) -> Vec<&str> {
    page.lines.iter().map(|line| line.text.as_str()).collect()
}

fn cursors(page: &ConsolePage) -> Vec<u64> {
    page.lines.iter().map(|line| line.cursor).collect()
}

fn full_page(capture: &ConsoleCapture) -> ConsolePage {
    capture
        .snapshot(0, MAX_PAGE_LINES)
        .expect("one bounded page")
}

/// Collect a bounded sample across real byte-limited pages, without treating a
/// requested line limit as a promise that all those lines fit in one page.
fn sample_lines(capture: &ConsoleCapture, mut after: u64, limit: usize) -> Vec<ConsoleLine> {
    assert!((1..=MAX_RETAINED_LINES).contains(&limit));
    let mut lines = Vec::new();
    while lines.len() < limit {
        let page = capture
            .snapshot(after, (limit - lines.len()).min(MAX_PAGE_LINES))
            .expect("bounded sample page");
        assert!(page.lines.iter().map(|line| line.text.len()).sum::<usize>() <= MAX_PAGE_BYTES);
        if page.lines.is_empty() {
            break;
        }
        assert!(page.next_cursor > after, "nonempty page must advance");
        after = page.next_cursor;
        lines.extend(page.lines);
    }
    lines
}

/// Counts what the capture actually retains by paging with the public API only.
fn retained_line_count(capture: &ConsoleCapture) -> usize {
    let mut count = 0;
    let mut after = 0;
    loop {
        let page = capture
            .snapshot(after, MAX_PAGE_LINES)
            .expect("bounded page");
        if page.lines.is_empty() {
            return count;
        }
        count += page.lines.len();
        assert!(page.next_cursor > after, "nonempty page must advance");
        after = page.next_cursor;
    }
}

#[test]
fn delimiters_terminate_lines_across_reads_and_flush_unterminated_tails() {
    let capture = ConsoleCapture::new();

    // LF, CRLF and a bare CR are all delimiters, and the CRLF pair yields exactly
    // one line even when the two bytes are separated by a read boundary.
    let report = capture
        .capture_reader(
            ConsoleStream::Stdout,
            ChunkedReader::new(b"alpha\nbeta\r\ngamma\r".to_vec(), 1),
        )
        .expect("delimited capture");
    assert_eq!(
        report,
        CaptureReport {
            lines: 3,
            truncated_lines: 0
        }
    );
    let page = full_page(&capture);
    assert_eq!(line_texts(&page), vec!["alpha", "beta", "gamma"]);
    assert_eq!(page.latest_cursor, 3);
    assert_eq!(page.next_cursor, 3);
    assert!(!page.gap);

    // Empty lines are retained, not folded away: two delimiters are two lines.
    let blanks = ConsoleCapture::new();
    let report = blanks
        .capture_reader(ConsoleStream::Stdout, Cursor::new(b"\n\n".to_vec()))
        .expect("empty line capture");
    assert_eq!(report.lines, 2);
    assert_eq!(report.truncated_lines, 0);
    assert_eq!(line_texts(&full_page(&blanks)), vec!["", ""]);
    assert!(full_page(&blanks).lines.iter().all(|line| !line.truncated));

    // A buffer completed by its own trailing delimiter is published once, and EOF
    // with an empty buffer adds nothing further.
    let complete = ConsoleCapture::new();
    let report = complete
        .capture_reader(ConsoleStream::Stdout, Cursor::new(b"only\n".to_vec()))
        .expect("complete line capture");
    assert_eq!(report.lines, 1);
    assert_eq!(line_texts(&full_page(&complete)), vec!["only"]);

    // A non-empty buffer at EOF is flushed as a final unterminated line.
    let unterminated = ConsoleCapture::new();
    let report = unterminated
        .capture_reader(ConsoleStream::Stdout, Cursor::new(b"no delimiter".to_vec()))
        .expect("unterminated capture");
    assert_eq!(
        report,
        CaptureReport {
            lines: 1,
            truncated_lines: 0
        }
    );
    let page = full_page(&unterminated);
    assert_eq!(line_texts(&page), vec!["no delimiter"]);
    assert_eq!(page.latest_cursor, 1);
}

#[test]
fn publication_bounds_keep_scalars_and_drain_excess_bytes_to_the_marker() {
    let capture = ConsoleCapture::new();

    // Exactly at the publication bound: retained whole and never marked truncated.
    let mut at_bound = vec![b'z'; MAX_LINE_BYTES];
    at_bound.push(b'\n');
    let report = capture
        .capture_reader(ConsoleStream::Stdout, Cursor::new(at_bound))
        .expect("exact-bound capture");
    assert_eq!(
        report,
        CaptureReport {
            lines: 1,
            truncated_lines: 0
        }
    );
    let line = full_page(&capture).lines[0].clone();
    assert_eq!(line.text.len(), MAX_LINE_BYTES);
    assert_eq!(line.text, "z".repeat(MAX_LINE_BYTES));
    assert!(!line.truncated);

    // The overflow input spans read-chunk boundaries, so the drain continues across
    // reads and the closing delimiter is still recognized on the next chunk.
    let overflow = ConsoleCapture::new();
    let mut over = vec![b'y'; MAX_LINE_BYTES + 1];
    over.extend_from_slice(b"\nAFTER-MARKER\n");
    let report = overflow
        .capture_reader(ConsoleStream::Stdout, Cursor::new(over))
        .expect("overflow capture");
    assert_eq!(
        report,
        CaptureReport {
            lines: 2,
            truncated_lines: 1
        }
    );
    let lines = sample_lines(&overflow, 0, 2);
    assert_eq!(lines[0].text.len(), MAX_LINE_BYTES);
    assert_eq!(lines[0].text, "y".repeat(MAX_LINE_BYTES));
    assert!(lines[0].truncated, "one dropped byte must be reported");
    assert_eq!(lines[0].cursor, 1);
    assert_eq!(lines[1].text, "AFTER-MARKER");
    assert!(!lines[1].truncated, "the marker line is complete");
    assert_eq!(lines[1].cursor, 2);

    // Three- and four-byte scalars fitting exactly are retained. Every crossing
    // offset drops the scalar whole, including when its bytes arrive separately.
    for scalar in ["€", "🦀"] {
        for crossing in 0..scalar.len() {
            let prefix = "a".repeat(MAX_LINE_BYTES - scalar.len() + crossing);
            let input = format!("{prefix}{scalar}\nMARKER\n");
            let boundary = ConsoleCapture::new();
            let report = boundary
                .capture_reader(
                    ConsoleStream::Stdout,
                    ChunkedReader::new(input.into_bytes(), 1),
                )
                .expect("split scalar capture");
            let lines = sample_lines(&boundary, 0, 2);
            assert_eq!(report.lines, 2);
            assert_eq!(report.truncated_lines, u64::from(crossing > 0));
            assert_eq!(
                lines[0].text,
                if crossing == 0 {
                    format!("{prefix}{scalar}")
                } else {
                    prefix
                }
            );
            assert_eq!(lines[0].truncated, crossing > 0);
            assert!(lines[0].text.len() <= MAX_LINE_BYTES);
            assert!(!lines[0].text.contains('\u{fffd}'));
            assert_eq!(lines[1].text, "MARKER");
            assert!(!lines[1].truncated);
        }
    }

    // Delimiters are always recognized after a drained excess: the read chunk does
    // not consume the delimiter that closes the overlong line.
    let chunked = ConsoleCapture::new();
    let mut over = vec![b'c'; MAX_LINE_BYTES + 1 + READ_BYTES];
    over.extend_from_slice(b"\nTAIL\n");
    let report = chunked
        .capture_reader(ConsoleStream::Stdout, Cursor::new(over))
        .expect("chunked excess capture");
    assert_eq!(
        report,
        CaptureReport {
            lines: 2,
            truncated_lines: 1
        }
    );
    let lines = sample_lines(&chunked, 0, 2);
    assert_eq!(lines[0].text.len(), MAX_LINE_BYTES);
    assert_eq!(lines[1].text, "TAIL");
    assert!(!lines[1].truncated);
}

#[test]
fn invalid_utf8_is_replaced_and_never_published_as_raw_bytes() {
    let capture = ConsoleCapture::new();

    // Every invalid byte becomes the replacement character rather than raw text,
    // and one published line has exactly one cursor.
    let report = capture
        .capture_reader(
            ConsoleStream::Stderr,
            Cursor::new(vec![0xFF, b'o', b'k', 0xFE, b'\n']),
        )
        .expect("invalid utf-8 capture");
    assert_eq!(
        report,
        CaptureReport {
            lines: 1,
            truncated_lines: 0
        }
    );
    let page = full_page(&capture);
    assert_eq!(line_texts(&page), vec!["\u{fffd}ok\u{fffd}"]);
    assert_eq!(page.lines[0].stream, ConsoleStream::Stderr);
    assert_eq!(page.lines[0].cursor, 1);
    assert!(!page.lines[0].truncated);

    // A truncated multi-byte scalar is one replacement character, not raw bytes.
    let cut = ConsoleCapture::new();
    let mut bytes = vec![b'p'];
    bytes.extend_from_slice(&[0xE2, 0x82]);
    bytes.push(b'\n');
    let report = cut
        .capture_reader(ConsoleStream::Stdout, Cursor::new(bytes))
        .expect("truncated scalar capture");
    assert_eq!(
        report,
        CaptureReport {
            lines: 1,
            truncated_lines: 0
        }
    );
    assert_eq!(line_texts(&full_page(&cut)), vec!["p\u{fffd}"]);

    // Invalid bytes expand threefold at decode time, so far more than the bound is
    // discarded: publication rounds down to whole scalars within the byte bound.
    let expansion = ConsoleCapture::new();
    let raw_count = MAX_LINE_BYTES + READ_BYTES;
    let mut bytes = vec![0xFF; raw_count];
    bytes.push(b'\n');
    let report = expansion
        .capture_reader(ConsoleStream::Stdout, Cursor::new(bytes))
        .expect("expansion capture");
    assert_eq!(
        report,
        CaptureReport {
            lines: 1,
            truncated_lines: 1
        }
    );
    let line = full_page(&expansion).lines[0].clone();
    assert_eq!(
        line.text.len(),
        3 * (MAX_LINE_BYTES / 3),
        "replacement expansion rounds down without splitting a scalar"
    );
    assert_eq!(line.text, "\u{fffd}".repeat(MAX_LINE_BYTES / 3));
    assert!(line.truncated);
    assert!(line.text.chars().all(|value| value == '\u{fffd}'));
}

#[test]
fn interrupted_reads_are_retried_and_failures_publish_no_incomplete_line() {
    let capture = ConsoleCapture::new();
    let reader = InterruptedReader::new(b"kept\n".to_vec(), 3);
    let report = capture
        .capture_reader(ConsoleStream::Stdout, reader)
        .expect("interrupted reads are transparent");
    assert_eq!(
        report,
        CaptureReport {
            lines: 1,
            truncated_lines: 0
        }
    );
    assert_eq!(line_texts(&full_page(&capture)), vec!["kept"]);

    // Completed lines remain published; only the incomplete failed suffix is lost.
    let failing = ConsoleCapture::new();
    let error = failing
        .capture_reader(
            ConsoleStream::Stderr,
            FailingReader::new(
                b"complete\npartial".to_vec(),
                io::Error::other("pipe fault"),
            ),
        )
        .expect_err("a read failure must be reported");
    assert_eq!(error, ConsoleError::ReadFailed);
    assert_eq!(error.code(), "console_read_failed");
    let page = full_page(&failing);
    assert_eq!(line_texts(&page), vec!["complete"]);
    assert_eq!(page.latest_cursor, 1);
    assert_eq!(page.next_cursor, 1);
    assert!(!page.gap);

    // The same capture keeps working after a failed reader: a failed capture leaves
    // no wedged storage and no partially framed line behind.
    let report = failing
        .capture_reader(ConsoleStream::Stderr, Cursor::new(b"after\n".to_vec()))
        .expect("capture after failure");
    assert_eq!(report.lines, 1);
    assert_eq!(line_texts(&full_page(&failing)), vec!["complete", "after"]);
}

/// Answers `Interrupted` a fixed number of times before serving its data.
struct InterruptedReader {
    data: Cursor<Vec<u8>>,
    pending: usize,
}
impl InterruptedReader {
    fn new(data: Vec<u8>, pending: usize) -> Self {
        Self {
            data: Cursor::new(data),
            pending,
        }
    }
}
impl Read for InterruptedReader {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if self.pending > 0 {
            self.pending -= 1;
            return Err(io::Error::from(io::ErrorKind::Interrupted));
        }
        self.data.read(buffer)
    }
}

/// Serves all bytes, then fails with the planted error instead of clean EOF.
struct FailingReader {
    data: Cursor<Vec<u8>>,
    failure: Option<io::Error>,
}
impl FailingReader {
    fn new(data: Vec<u8>, failure: io::Error) -> Self {
        Self {
            data: Cursor::new(data),
            failure: Some(failure),
        }
    }
}
impl Read for FailingReader {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let read = self.data.read(buffer)?;
        if read == 0 && !buffer.is_empty() {
            return Err(self
                .failure
                .take()
                .expect("reader must stop on the first hard failure"));
        }
        Ok(read)
    }
}

#[test]
fn snapshots_page_without_skipping_across_both_streams() {
    let capture = ConsoleCapture::new();
    // Two streams share one capture, so storage and cursor order are global.
    capture
        .capture_reader(ConsoleStream::Stdout, Cursor::new(b"out-1\n".to_vec()))
        .expect("stdout capture");
    capture
        .capture_reader(ConsoleStream::Stderr, Cursor::new(b"err-1\n".to_vec()))
        .expect("stderr capture");
    capture
        .capture_reader(ConsoleStream::Stdout, Cursor::new(b"out-2\n".to_vec()))
        .expect("stdout capture");

    let first = capture.snapshot(0, 2).expect("first page");
    assert_eq!(line_texts(&first), vec!["out-1", "err-1"]);
    assert_eq!(first.next_cursor, 2, "the cursor is the last returned line");
    assert_eq!(
        first.latest_cursor, 3,
        "the latest cursor is ahead of a short page"
    );
    assert!(!first.gap);
    assert_eq!(first.lines[0].stream, ConsoleStream::Stdout);
    assert_eq!(first.lines[1].stream, ConsoleStream::Stderr);

    // Resuming at next_cursor cannot skip retained work; the third line is reached
    // exactly once however the page is walked.
    let second = capture
        .snapshot(first.next_cursor, 2)
        .expect("resumed page");
    assert_eq!(line_texts(&second), vec!["out-2"]);
    assert_eq!(second.next_cursor, 3);
    assert_eq!(second.latest_cursor, 3);
    assert!(!second.gap);
    assert!(
        capture
            .snapshot(second.next_cursor, 2)
            .expect("tail")
            .lines
            .is_empty()
    );

    // A wrong resume point can skip retained work.
    let skipped = capture
        .snapshot(first.latest_cursor, 2)
        .expect("latest-cursor page");
    assert!(
        skipped.lines.is_empty(),
        "the latest cursor is not a page cursor"
    );
    assert_eq!(skipped.next_cursor, 3);

    // Cursors are contiguous and monotonic across captures; time is observed, never
    // caller supplied, and never in the future.
    let now = SystemTime::now();
    let mut cursor = 0;
    let mut observed = Vec::new();
    while cursor < 3 {
        let page = capture.snapshot(cursor, 1).expect("single-line page");
        let line = page.lines.first().expect("a retained line remains");
        assert_eq!(line.cursor, cursor + 1, "cursors are contiguous");
        assert!(
            line.observed_at <= now,
            "observation time is not in the future"
        );
        observed.push((line.stream, line.text.clone()));
        cursor = line.cursor;
    }
    assert_eq!(
        observed,
        vec![
            (ConsoleStream::Stdout, "out-1".to_owned()),
            (ConsoleStream::Stderr, "err-1".to_owned()),
            (ConsoleStream::Stdout, "out-2".to_owned()),
        ],
        "single-line paging preserves order, text and stream"
    );
}

#[test]
fn retention_bounds_and_failures_report_fixed_redacted_errors() {
    // Line-count bound. One byte per line, so the byte bound cannot fire.
    let by_count = ConsoleCapture::new();
    let report = by_count
        .capture_reader(
            ConsoleStream::Stdout,
            RepeatingReader::new(b"x\n".to_vec(), MAX_RETAINED_LINES + 5),
        )
        .expect("line-count retention capture");
    assert_eq!(report.lines, (MAX_RETAINED_LINES + 5) as u64);
    assert_eq!(report.truncated_lines, 0);
    assert_eq!(retained_line_count(&by_count), MAX_RETAINED_LINES);
    assert_eq!(MAX_RETAINED_LINES, 10_000);
    assert_eq!(MAX_RETAINED_BYTES, 2 * 1024 * 1024);

    // The retained window is the newest one, and the evicted prefix is reported.
    let oldest = by_count.snapshot(0, 1).expect("oldest retained page");
    assert_eq!(oldest.lines[0].cursor, 6);
    assert_eq!(oldest.lines[0].text, "x");
    assert!(oldest.gap, "cursor zero reports the evicted prefix");
    assert!(!by_count.snapshot(5, 1).unwrap().gap);
    assert!(by_count.snapshot(4, 1).unwrap().gap);
    let evicted = by_count.snapshot(1, 1).expect("evicted prefix");
    assert!(evicted.gap, "an evicted requested prefix must be reported");
    assert_eq!(cursors(&evicted), vec![6]);
    assert_eq!(evicted.next_cursor, 6);
    assert_eq!(evicted.latest_cursor, MAX_RETAINED_LINES as u64 + 5);

    // Byte bound. Every line is exactly the publication bound, so the line-count
    // bound cannot fire before the byte bound does.
    let by_bytes = ConsoleCapture::new();
    let mut chunk = vec![b'z'; MAX_LINE_BYTES];
    chunk.push(b'\n');
    let report = by_bytes
        .capture_reader(ConsoleStream::Stdout, RepeatingReader::new(chunk, 200))
        .expect("byte retention capture");
    assert_eq!(report.lines, 200);
    assert_eq!(report.truncated_lines, 0);
    let retained = MAX_RETAINED_BYTES / MAX_LINE_BYTES;
    assert_eq!(retained_line_count(&by_bytes), retained);
    assert_eq!(
        retained * MAX_LINE_BYTES,
        MAX_RETAINED_BYTES,
        "the byte budget is consumed exactly, not exceeded"
    );
    assert!(
        retained < MAX_RETAINED_LINES,
        "the byte bound must be the one that binds here"
    );

    // The newest lines pushed the oldest window out, in cursor order.
    let bound_line = "z".repeat(MAX_LINE_BYTES);
    let oldest_three = by_bytes.snapshot(0, 3).expect("first retained page");
    let evicted = 200 - retained as u64;
    let first = evicted + 1;
    let page_capacity = MAX_PAGE_BYTES / MAX_LINE_BYTES;
    let first_count = 3.min(page_capacity).min(retained);
    assert_eq!(
        oldest_three
            .lines
            .iter()
            .map(|line| line.text.len())
            .sum::<usize>(),
        first_count * MAX_LINE_BYTES
    );
    assert_eq!(
        cursors(&oldest_three),
        (first..first + first_count as u64).collect::<Vec<_>>()
    );
    assert_eq!(
        line_texts(&oldest_three),
        vec![bound_line.as_str(); first_count]
    );
    assert!(oldest_three.gap);
    let straddling = by_bytes
        .snapshot(first + 1, 5)
        .expect("page straddling the boundary");
    let straddling_count = 5.min(page_capacity).min(retained - 2);
    assert_eq!(
        cursors(&straddling),
        (first + 2..first + 2 + straddling_count as u64).collect::<Vec<_>>()
    );
    assert!(!straddling.gap);
    assert!(!by_bytes.snapshot(evicted, 1).unwrap().gap);
    let evicted_prefix = by_bytes.snapshot(evicted - 1, 1).expect("evicted prefix");
    assert!(
        evicted_prefix.gap,
        "an evicted requested prefix must be reported"
    );
    assert_eq!(cursors(&evicted_prefix), vec![first]);
    assert_eq!(evicted_prefix.next_cursor, first);
    assert_eq!(evicted_prefix.latest_cursor, 200);

    // Invalid page arguments are refused with one fixed code, never clamped.
    assert_eq!(MAX_PAGE_LINES, 200);
    for (after, limit) in [(0, 0), (0, MAX_PAGE_LINES + 1), (0, usize::MAX)] {
        let error = by_bytes
            .snapshot(after, limit)
            .expect_err("invalid page refused");
        assert_eq!(error, ConsoleError::InvalidPage);
        assert_eq!(error.code(), "console_invalid_page");
    }
    let exact = by_bytes
        .snapshot(0, MAX_PAGE_LINES)
        .expect("exact bound accepted");
    assert_eq!(exact.lines.len(), retained.min(page_capacity));
    assert_eq!(
        by_count.snapshot(0, MAX_PAGE_LINES).unwrap().lines.len(),
        MAX_PAGE_LINES
    );
    // A cursor beyond the latest retained cursor is refused, not clamped to it.
    assert_eq!(
        by_bytes
            .snapshot(exact.latest_cursor + 1, 1)
            .expect_err("future cursor"),
        ConsoleError::InvalidPage
    );
    assert_eq!(
        by_bytes
            .snapshot(exact.latest_cursor, 1)
            .expect("latest cursor accepted"),
        ConsolePage {
            lines: Vec::new(),
            next_cursor: exact.latest_cursor,
            latest_cursor: exact.latest_cursor,
            gap: false,
        }
    );

    // A planted native I/O failure message never reaches the error surface, and the
    // fixed Display carries no source detail.
    let error = ConsoleCapture::new()
        .capture_reader(
            ConsoleStream::Stdout,
            FailingReader::new(
                b"secret-payload\n".to_vec(),
                io::Error::other("native-failure-sentinel-7b41 at C:\\secret\\pipe"),
            ),
        )
        .expect_err("read failure");
    assert_eq!(error, ConsoleError::ReadFailed);
    assert_eq!(error.code(), "console_read_failed");
    assert_eq!(error.to_string(), "Console capture failed");
    for rendered in [format!("{error:?}"), error.to_string()] {
        for sentinel in [
            "native-failure-sentinel-7b41",
            "secret-payload",
            "C:\\secret\\pipe",
        ] {
            assert!(
                !rendered.contains(sentinel),
                "error surface leaked {sentinel}: {rendered}"
            );
        }
    }
    assert!(std::error::Error::source(&error).is_none());

    // A distinct real payload must not appear in diagnostics, but remains raw in snapshots.
    let private = ConsoleCapture::new();
    private
        .capture_reader(ConsoleStream::Stdout, Cursor::new(b"console-secret-2c84\n"))
        .unwrap();
    let page = full_page(&private);
    let line = page.lines[0].clone();
    assert_eq!(line.text, "console-secret-2c84");
    for rendered in [
        format!("{line:?}"),
        format!("{page:?}"),
        format!("{private:?}"),
    ] {
        assert!(!rendered.contains("console-secret-2c84"));
    }
    assert!(format!("{line:?}").contains("text_bytes"));
    assert!(matches!(line, ConsoleLine { cursor: _, .. }));
}
