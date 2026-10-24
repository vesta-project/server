//! Framing tests with independent literal expectations.
//!
//! Expected bytes are written out as literals rather than recomputed from the code
//! under test, and every bound is exercised at the exact limit and one unit past it.
//! A test that derived its prefix from the same arithmetic as `write_frame` would pass
//! even if the prefix were wrong.

use super::{
    FrameBody, FrameError, FrameLimit, MAX_CONTROL_FRAME_BYTES, MAX_FRAME_BYTES, read_frame,
    write_frame,
};
use std::cell::Cell;
use std::fmt::Debug;
use std::io::{self, ErrorKind, Read, Write};

/// A reader that hands out data in awkward pieces and can be made to misbehave.
///
/// Two of its behaviours are load-bearing for the tests below. `poisoned_after` caps
/// every read at the bound and panics on any read attempted at or past it, so a limit
/// check that ran *after* touching the body would abort the test rather than pass it.
/// The separate failing reader/writer fixtures likewise cap individual calls so
/// their configured failure offsets cannot be skipped.
struct Chunked {
    data: Vec<u8>,
    chunk: usize,
    cursor: usize,
    interrupt_at: Option<usize>,
    poison_after: Option<usize>,
}
impl Chunked {
    fn new(data: impl Into<Vec<u8>>) -> Self {
        Self {
            data: data.into(),
            chunk: usize::MAX,
            cursor: 0,
            interrupt_at: None,
            poison_after: None,
        }
    }
    fn chunked(mut self, chunk: usize) -> Self {
        self.chunk = chunk.max(1);
        self
    }
    fn interrupting_once(mut self, at: usize) -> Self {
        self.interrupt_at = Some(at);
        self
    }
    fn poisoned_after(mut self, offset: usize) -> Self {
        self.poison_after = Some(offset);
        self
    }
    fn consumed(&self) -> usize {
        self.cursor
    }
}
impl Read for Chunked {
    fn read(&mut self, into: &mut [u8]) -> io::Result<usize> {
        let mut into = into;
        if let Some(bound) = self.poison_after {
            assert!(
                self.cursor < bound,
                "reader was read at byte {}, at or past the poisoned bound {}",
                self.cursor,
                bound
            );
            let allowance = (bound - self.cursor).min(into.len());
            into = &mut into[..allowance];
        }
        if let Some(at) = self.interrupt_at
            && self.cursor == at
        {
            self.interrupt_at = None;
            return Err(io::Error::new(ErrorKind::Interrupted, "simulated"));
        }
        if self.cursor >= self.data.len() || into.is_empty() {
            return Ok(0);
        }
        // Derived from the remaining length, so no addition can wrap.
        let count = into
            .len()
            .min(self.chunk)
            .min(self.data.len() - self.cursor);
        into[..count].copy_from_slice(&self.data[self.cursor..self.cursor + count]);
        self.cursor += count;
        Ok(count)
    }
}

/// A writer that records output, capped to a byte budget it then refuses to exceed.
struct Recorder {
    out: Vec<u8>,
    chunk: usize,
    fail_after: Option<usize>,
    interrupt_at: Option<usize>,
}
impl Recorder {
    fn new() -> Self {
        Self {
            out: Vec::new(),
            chunk: usize::MAX,
            fail_after: None,
            interrupt_at: None,
        }
    }
    fn chunked(mut self, chunk: usize) -> Self {
        self.chunk = chunk.max(1);
        self
    }
    /// Refuse once `offset` bytes are recorded, capping each write so the bound is hit
    /// exactly rather than overshot inside a single call.
    fn failing_after(mut self, offset: usize) -> Self {
        self.fail_after = Some(offset);
        self
    }
    fn interrupting_once(mut self, at: usize) -> Self {
        self.interrupt_at = Some(at);
        self
    }
}
impl Write for Recorder {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if let Some(at) = self.interrupt_at
            && self.out.len() == at
        {
            self.interrupt_at = None;
            return Err(io::Error::new(ErrorKind::Interrupted, "simulated"));
        }
        let mut count = buf.len().min(self.chunk);
        if let Some(limit) = self.fail_after {
            if self.out.len() >= limit {
                return Err(io::Error::other("simulated writer failure"));
            }
            count = count.min(limit - self.out.len());
        }
        self.out.extend_from_slice(&buf[..count]);
        Ok(count)
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// A reader that fails once a byte budget is reached, capped so the bound is exact.
struct FailingReader {
    inner: Chunked,
    after: usize,
    message: &'static str,
}
impl Read for FailingReader {
    fn read(&mut self, into: &mut [u8]) -> io::Result<usize> {
        if self.inner.cursor >= self.after {
            return Err(io::Error::other(self.message));
        }
        let allowance = (self.after - self.inner.cursor).min(into.len());
        self.inner.read(&mut into[..allowance])
    }
}

/// A writer that accepts no bytes.
struct ZeroWriter;
impl Write for ZeroWriter {
    fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
        Ok(0)
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn small_limit() -> FrameLimit {
    FrameLimit::new(64).expect("64 is a usable limit")
}

fn usable(bound: usize) -> FrameLimit {
    FrameLimit::new(bound).unwrap_or_else(|error| panic!("{bound} must be usable: {error:?}"))
}

#[track_caller]
fn assert_refused<T: Debug>(result: Result<T, FrameError>, expected: FrameError) {
    match result {
        Err(actual) => assert_eq!(actual, expected),
        Ok(value) => panic!("expected {expected:?}, produced {value:?}"),
    }
}

/// Build a framed stream of literal bytes for a body of `body.len()` bytes.
fn framed(body: &str) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&(body.len() as u32).to_be_bytes());
    bytes.extend_from_slice(body.as_bytes());
    bytes
}

/// A hand-written frame decodes to the exact text it carries.
#[test]
fn a_literal_golden_frame_decodes_to_its_text() {
    // 00 00 00 02 "hi"
    let bytes = [0x00, 0x00, 0x00, 0x02, b'h', b'i'];
    let frame = read_frame(&mut Chunked::new(bytes.to_vec()), small_limit())
        .expect("a complete frame reads")
        .expect("a frame is present");
    assert_eq!(frame.as_str(), "hi");
    assert_eq!(frame.as_bytes(), b"hi");
}

/// Writing produces exactly the hand-checked prefix and body, in that order.
#[test]
fn writing_produces_the_literal_prefix_and_body() {
    let mut out = Recorder::new();
    write_frame(&mut out, "hi", small_limit()).expect("a small body writes");
    assert_eq!(out.out, vec![0x00, 0x00, 0x00, 0x02, b'h', b'i']);
}

/// The prefix counts UTF-8 bytes, not characters.
#[test]
fn the_prefix_counts_bytes_rather_than_characters() {
    // "é" is two UTF-8 bytes (0xC3 0xA9), so the declared length is 2, not 1.
    let mut out = Recorder::new();
    write_frame(&mut out, "é", small_limit()).expect("a multibyte body writes");
    assert_eq!(out.out, vec![0x00, 0x00, 0x00, 0x02, 0xC3, 0xA9]);

    let frame = read_frame(&mut Chunked::new(out.out.clone()), small_limit())
        .expect("a multibyte frame reads")
        .expect("a frame is present");
    assert_eq!(frame.as_str(), "é");
    assert_eq!(frame.as_bytes().len(), 2);
}

/// A limit is usable anywhere in `1..=MAX_FRAME_BYTES` and nowhere else.
#[test]
fn limits_are_bounded_at_both_ends() {
    assert_eq!(usable(1).max_body_bytes(), 1);
    assert_eq!(usable(MAX_FRAME_BYTES).max_body_bytes(), MAX_FRAME_BYTES);
    assert_refused(FrameLimit::new(0), FrameError::InvalidLimit);
    assert_refused(
        FrameLimit::new(MAX_FRAME_BYTES + 1),
        FrameError::InvalidLimit,
    );
    assert_refused(FrameLimit::new(usize::MAX), FrameError::InvalidLimit);
}

/// The two documented channel bounds are the values the Java codecs used.
#[test]
fn the_documented_channel_bounds_are_exact() {
    assert_eq!(MAX_FRAME_BYTES, 16_777_216);
    assert_eq!(MAX_CONTROL_FRAME_BYTES, 1_048_576);
    assert_eq!(
        usable(MAX_CONTROL_FRAME_BYTES).max_body_bytes(),
        MAX_CONTROL_FRAME_BYTES
    );
}

/// A declared length above the limit is refused after four bytes, with no body read.
#[test]
fn an_oversized_declared_length_is_refused_before_the_body_is_touched() {
    // Declares 65 against a 64-byte limit, and supplies nothing afterwards.
    let mut reader = Chunked::new(vec![0x00, 0x00, 0x00, 0x41]).poisoned_after(4);
    assert_refused(
        read_frame(&mut reader, small_limit()),
        FrameError::BodyTooLarge,
    );
    assert_eq!(reader.consumed(), 4, "only the prefix may be consumed");
}

/// The two largest u32 lengths are refused without allocating, on any limit.
#[test]
fn the_largest_declared_lengths_are_refused_without_touching_a_body() {
    for declared in [0xFFFF_FFFFu32, 0x8000_0000] {
        let mut reader = Chunked::new(declared.to_be_bytes().to_vec()).poisoned_after(4);
        assert_refused(
            read_frame(&mut reader, usable(MAX_CONTROL_FRAME_BYTES)),
            FrameError::BodyTooLarge,
        );
        assert_eq!(reader.consumed(), 4);
    }
}

/// A zero-length frame is refused, not reported as an empty message.
#[test]
fn a_zero_length_frame_is_refused_rather_than_read_as_empty() {
    let mut reader = Chunked::new(vec![0x00, 0x00, 0x00, 0x00]).poisoned_after(4);
    assert_refused(
        read_frame(&mut reader, usable(MAX_FRAME_BYTES)),
        FrameError::EmptyBody,
    );
    assert_eq!(reader.consumed(), 4);
}

/// An empty outgoing body is refused before the prefix is written.
#[test]
fn an_empty_outgoing_body_is_refused_before_anything_is_written() {
    let mut out = Recorder::new();
    assert_refused(
        write_frame(&mut out, "", small_limit()),
        FrameError::EmptyBody,
    );
    assert!(out.out.is_empty(), "no prefix may be written");
}

/// A small outgoing bound is inclusive and refuses exactly one byte more.
#[test]
fn the_outgoing_bound_is_inclusive_and_refuses_one_byte_more() {
    let limit = usable(8);
    let mut at_bound = Recorder::new();
    write_frame(&mut at_bound, "aaaaaaaa", limit).expect("exactly the limit is accepted");
    assert_eq!(at_bound.out[0..4], [0x00, 0x00, 0x00, 0x08]);

    let mut over = Recorder::new();
    assert_refused(
        write_frame(&mut over, "aaaaaaaaa", limit),
        FrameError::BodyTooLarge,
    );
    assert!(over.out.is_empty(), "no prefix may be written");
}

/// Both documented channel bounds work as exact outgoing limits.
#[test]
fn each_documented_channel_bound_is_exact_when_writing() {
    for bound in [MAX_CONTROL_FRAME_BYTES, MAX_FRAME_BYTES] {
        let limit = usable(bound);
        let body = "n".repeat(bound);

        let mut at_bound = Recorder::new();
        write_frame(&mut at_bound, &body, limit).unwrap_or_else(|error| {
            panic!("a body of exactly {bound} bytes must be accepted, got {error:?}")
        });
        assert_eq!(
            u32::from_be_bytes(at_bound.out[0..4].try_into().expect("a four byte prefix")),
            bound as u32,
            "the prefix must declare the byte count {bound}"
        );
        assert_eq!(at_bound.out.len(), bound + 4);
        drop(at_bound);

        let mut over = Recorder::new();
        assert_refused(
            write_frame(&mut over, &format!("{body}n"), limit),
            FrameError::BodyTooLarge,
        );
        assert!(over.out.is_empty(), "no prefix may be written");
    }
}

/// Both documented channel bounds work as exact incoming limits.
#[test]
fn each_documented_channel_bound_is_exact_when_reading() {
    for bound in [MAX_CONTROL_FRAME_BYTES, MAX_FRAME_BYTES] {
        let limit = usable(bound);
        let stream = framed(&"n".repeat(bound));

        // Exactly at the bound: accepted, having read the whole stream.
        let mut reader = Chunked::new(stream.clone());
        let frame = read_frame(&mut reader, limit)
            .unwrap_or_else(|error| {
                panic!("a body of exactly {bound} bytes must be accepted, got {error:?}")
            })
            .expect("a frame is present");
        assert_eq!(frame.as_bytes().len(), bound);
        assert_eq!(reader.consumed(), bound + 4);

        // One byte more: refused after the prefix, with no body read at all.
        let mut reader = Chunked::new((bound as u32 + 1).to_be_bytes().to_vec()).poisoned_after(4);
        assert_refused(read_frame(&mut reader, limit), FrameError::BodyTooLarge);
        assert_eq!(reader.consumed(), 4);
    }
}

/// A large body written one byte at a time frames correctly.
///
/// This is a regression guard for partial-write handling: a caller that recursed per
/// byte would exhaust the stack well before 64 KiB, while a caller that lost a
/// remainder would emit a short frame.
#[test]
fn a_large_body_written_one_byte_at_a_time_stays_framed() {
    const LEN: usize = 64 * 1024;
    let body = "s".repeat(LEN);

    let mut out = Recorder::new().chunked(1);
    write_frame(&mut out, &body, usable(LEN)).expect("a one-byte writer still completes");

    assert_eq!(
        u32::from_be_bytes(out.out[0..4].try_into().expect("a four byte prefix")),
        LEN as u32
    );
    assert_eq!(out.out.len(), LEN + 4);
    assert_eq!(out.out, framed(&body));

    let mut reader = Chunked::new(out.out.clone()).chunked(1);
    let frame = read_frame(&mut reader, usable(LEN))
        .expect("a byte-at-a-time frame reads")
        .expect("a frame is present");
    assert_eq!(frame.as_bytes().len(), LEN);
}

/// A close before any byte is a clean end; every other short prefix is a truncation.
#[test]
fn only_a_close_before_the_first_byte_is_a_clean_end() {
    assert!(
        read_frame(&mut Chunked::new(Vec::new()), small_limit())
            .expect("a clean close is not an error")
            .is_none(),
        "an empty stream carries no frame"
    );

    let full = [0x00, 0x00, 0x00, 0x02, b'h', b'i'];
    for taken in 1..4 {
        let mut reader = Chunked::new(full[..taken].to_vec());
        assert_refused(
            read_frame(&mut reader, small_limit()),
            FrameError::TruncatedPrefix,
        );
        assert_eq!(reader.consumed(), taken);
    }
}

/// A body that ends early is a truncation at every offset.
#[test]
fn a_close_inside_the_body_is_a_truncated_body_at_every_offset() {
    let full = [0x00, 0x00, 0x00, 0x05, b'a', b'b', b'c', b'd', b'e'];
    for supplied in 0..5 {
        let mut reader = Chunked::new(full[..4 + supplied].to_vec());
        assert_refused(
            read_frame(&mut reader, small_limit()),
            FrameError::TruncatedBody,
        );
        assert_eq!(reader.consumed(), 4 + supplied);
    }
}

/// One byte at a time still produces the same bytes on the wire.
#[test]
fn one_byte_reads_and_writes_produce_identical_framing() {
    let body = "a moderately long body, written and read one byte at a time";
    let mut out = Recorder::new().chunked(1);
    write_frame(&mut out, body, small_limit()).expect("byte-at-a-time writing works");

    let expected = framed(body);
    assert_eq!(out.out, expected);

    let mut reader = Chunked::new(expected).chunked(1);
    let frame = read_frame(&mut reader, small_limit())
        .expect("byte-at-a-time reading works")
        .expect("a frame is present");
    assert_eq!(frame.as_str(), body);
    assert_eq!(reader.consumed(), body.len() + 4);
}

/// An interrupt at every byte offset is retried, not reported as a broken stream.
///
/// Both sides are chunked to one byte so that every offset is genuinely reached; with a
/// single large write most of these offsets would never be visited at all.
#[test]
fn an_interrupt_at_every_offset_is_retried_rather_than_failing_the_frame() {
    let bytes = framed("hi");
    let total = bytes.len();

    for at in 0..total {
        let mut reader = Chunked::new(bytes.clone()).chunked(1).interrupting_once(at);
        let frame = read_frame(&mut reader, small_limit())
            .unwrap_or_else(|error| panic!("a read interrupt at {at} became {error:?}"))
            .unwrap_or_else(|| panic!("a read interrupt at {at} lost the frame"));
        assert_eq!(frame.as_str(), "hi", "read interrupt at {at}");
        assert_eq!(reader.consumed(), total, "read interrupt at {at}");

        let mut out = Recorder::new().chunked(1).interrupting_once(at);
        write_frame(&mut out, "hi", small_limit())
            .unwrap_or_else(|error| panic!("a write interrupt at {at} became {error:?}"));
        assert_eq!(out.out, bytes, "write interrupt at {at}");
    }
}

/// A reader failure becomes a fixed error that carries no cause text.
#[test]
fn a_reader_failure_is_reported_without_its_cause() {
    let mut failing = FailingReader {
        inner: Chunked::new(framed("hi")),
        after: 1,
        message: "connection reset by peer while reading secret-token",
    };
    let error = read_frame(&mut failing, small_limit()).expect_err("a reader failure is an error");
    assert_eq!(error, FrameError::Io);
    assert!(!error.to_string().contains("secret-token"), "{error}");
}

/// Malformed UTF-8 is refused rather than repaired with a replacement character.
///
/// Each fixture declares exactly the bytes it supplies, so every one is a complete body
/// with an invalid encoding. A short body would be a truncation, not a decode failure.
#[test]
fn a_malformed_utf8_body_is_refused() {
    for bad in [
        // Declared 1, supplied 1: a lone continuation byte.
        vec![0x00, 0x00, 0x00, 0x01, 0xFF],
        // Declared 2, supplied 2: a lead byte with a non-continuation follower.
        vec![0x00, 0x00, 0x00, 0x02, 0xC3, 0x28],
        // Declared 3, supplied 3: a three-byte sequence cut short by a bad final byte.
        vec![0x00, 0x00, 0x00, 0x03, 0xE2, 0x82, 0x28],
        // Declared 3, supplied 3: an encoded surrogate half.
        vec![0x00, 0x00, 0x00, 0x03, 0xED, 0xA0, 0x80],
        // Declared 4, supplied 4: an overlong encoding of '/'.
        vec![0x00, 0x00, 0x00, 0x04, 0xC0, 0xAF, 0xC0, 0xAF],
    ] {
        assert_eq!(
            bad.len(),
            4 + u32::from_be_bytes(bad[0..4].try_into().expect("a four byte prefix")) as usize,
            "each fixture must supply exactly the body it declares"
        );
        let mut reader = Chunked::new(bad.clone());
        assert_refused(
            read_frame(&mut reader, small_limit()),
            FrameError::InvalidUtf8,
        );
        assert_eq!(reader.consumed(), bad.len());
    }
}

/// Two frames in one stream decode in order, and the second starts exactly after the first.
#[test]
fn two_concatenated_frames_decode_in_order() {
    let mut stream = Vec::new();
    stream.extend_from_slice(&[0x00, 0x00, 0x00, 0x02, b'h', b'i']);
    stream.extend_from_slice(&[0x00, 0x00, 0x00, 0x03, b'b', b'y', b'e']);

    let mut reader = Chunked::new(stream);
    let first = read_frame(&mut reader, small_limit())
        .expect("the first frame reads")
        .expect("the first frame is present");
    assert_eq!(first.as_str(), "hi");
    assert_eq!(reader.consumed(), 6);

    let second = read_frame(&mut reader, small_limit())
        .expect("the second frame reads")
        .expect("the second frame is present");
    assert_eq!(second.as_str(), "bye");
    assert_eq!(reader.consumed(), 13);

    assert!(
        read_frame(&mut reader, small_limit())
            .expect("the stream then closes cleanly")
            .is_none()
    );
}

/// A writer failure at any stage is reported.
///
/// The stream is unusable afterwards, so the assertion is only ever about the error and
/// the bytes that had already been handed out; nothing here promises reuse.
#[test]
fn writer_failures_at_the_prefix_and_body_are_reported() {
    // Refused before any byte is written.
    let mut untouched = Recorder::new().failing_after(0);
    assert_refused(
        write_frame(&mut untouched, "hi", small_limit()),
        FrameError::Io,
    );
    assert!(untouched.out.is_empty());

    // Refused part-way through the prefix.
    let mut mid_prefix = Recorder::new().failing_after(2);
    assert_refused(
        write_frame(&mut mid_prefix, "hi", small_limit()),
        FrameError::Io,
    );
    assert_eq!(mid_prefix.out, vec![0x00, 0x00]);

    // Refused after a complete prefix, with part of the body already out.
    let mut mid_body = Recorder::new().failing_after(5);
    assert_refused(
        write_frame(&mut mid_body, "hi", small_limit()),
        FrameError::Io,
    );
    assert_eq!(mid_body.out, vec![0x00, 0x00, 0x00, 0x02, b'h']);
}

/// A writer that accepts nothing is reported instead of being retried forever.
#[test]
fn a_writer_that_accepts_nothing_is_reported() {
    let mut stuck = ZeroWriter;
    assert_refused(write_frame(&mut stuck, "hi", small_limit()), FrameError::Io);
}

/// Errors are fixed strings with stable codes and no carried cause.
#[test]
fn errors_are_fixed_and_carry_no_cause() {
    let all = [
        FrameError::InvalidLimit,
        FrameError::EmptyBody,
        FrameError::BodyTooLarge,
        FrameError::TruncatedPrefix,
        FrameError::TruncatedBody,
        FrameError::InvalidUtf8,
        FrameError::Io,
    ];
    let mut codes = Vec::new();
    for error in all {
        assert_eq!(
            error.to_string(),
            format!("Framing failed: {}", error.code())
        );
        assert!(std::error::Error::source(&error).is_none(), "{error:?}");
        codes.push(error.code());
    }
    codes.sort_unstable();
    let before = codes.len();
    codes.dedup();
    assert_eq!(before, codes.len(), "every code is distinct");
    for code in codes {
        assert!(code.starts_with("frame_"), "{code}");
    }
}

/// The body is redacted in Debug but still reports its byte length.
#[test]
fn frame_body_debug_is_redacted_but_reports_its_length() {
    // "hidden-plain" is twelve bytes, so the prefix declares 0x0C.
    let secret = "hidden-plain";
    let bytes = framed(secret);
    assert_eq!(bytes.len(), 16);
    assert_eq!(&bytes[0..4], &[0x00, 0x00, 0x00, 0x0C]);

    let frame: FrameBody = read_frame(&mut Chunked::new(bytes), small_limit())
        .expect("a frame reads")
        .expect("a frame is present");
    assert_eq!(frame.as_str(), secret);

    let rendered = format!("{frame:?}");
    assert!(!rendered.contains("hidden"), "{rendered}");
    assert!(!rendered.contains("hidden-plain"), "{rendered}");
    assert!(rendered.contains("FrameBody"), "{rendered}");
    assert!(rendered.contains("len"), "{rendered}");
    // The reported length is the byte count, not the character count.
    assert!(rendered.contains("12"), "{rendered}");
}

#[test]
fn nonblocking_timeout_and_body_io_failures_refuse_without_retry_or_diagnostics() {
    struct Fault {
        kind: ErrorKind,
        after: usize,
        pos: usize,
        calls: usize,
    }
    impl Read for Fault {
        fn read(&mut self, into: &mut [u8]) -> io::Result<usize> {
            self.calls += 1;
            if self.pos == self.after {
                return Err(io::Error::new(self.kind, "secret peer diagnostic"));
            }
            let bytes = [0, 0, 0, 2, b'h', b'i'];
            let count = into.len().min(self.after - self.pos);
            into[..count].copy_from_slice(&bytes[self.pos..self.pos + count]);
            self.pos += count;
            Ok(count)
        }
    }
    impl Write for Fault {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.calls += 1;
            if self.pos == self.after {
                return Err(io::Error::new(self.kind, "secret peer diagnostic"));
            }
            let count = bytes.len().min(self.after - self.pos);
            self.pos += count;
            Ok(count)
        }
        fn flush(&mut self) -> io::Result<()> {
            panic!("framing must not flush")
        }
    }
    for kind in [
        ErrorKind::WouldBlock,
        ErrorKind::TimedOut,
        ErrorKind::ConnectionReset,
    ] {
        for after in [0, 2, 5] {
            let mut reader = Fault {
                kind,
                after,
                pos: 0,
                calls: 0,
            };
            let error = read_frame(&mut reader, small_limit()).unwrap_err();
            assert_eq!(error, FrameError::Io);
            assert_eq!(reader.pos, after);
            assert!(reader.calls <= 3);
            assert!(!error.to_string().contains("secret"));
            assert!(std::error::Error::source(&error).is_none());
            let mut writer = Fault {
                kind,
                after,
                pos: 0,
                calls: 0,
            };
            assert_refused(
                write_frame(&mut writer, "hi", small_limit()),
                FrameError::Io,
            );
            assert_eq!(writer.pos, after);
            assert!(writer.calls <= 3);
        }
    }
}

/// Writing leaves flushing to the caller.
#[test]
fn writing_leaves_flushing_to_the_caller() {
    struct CountingFlush(Cell<usize>);
    impl Write for CountingFlush {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            Ok(buf.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            self.0.set(self.0.get() + 1);
            Ok(())
        }
    }

    let mut counted = CountingFlush(Cell::new(0));
    write_frame(&mut counted, "hi", small_limit()).expect("a frame writes");
    assert_eq!(counted.0.get(), 0, "write_frame must not flush");
}
