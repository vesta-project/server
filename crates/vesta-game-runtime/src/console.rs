//! Bounded capture with no subscriber callbacks or viewer-dependent I/O.
//!
//! Reader ownership is synchronous: this module spawns no detached drain workers.
//! Future process supervision must own actual readers/threads through process exit.
use std::{
    collections::VecDeque,
    fmt,
    io::{ErrorKind, Read},
    sync::{Arc, Mutex},
    time::SystemTime,
};

pub const MAX_LINE_BYTES: usize = 16_384;
pub const MAX_RETAINED_LINES: usize = 10_000;
pub const MAX_RETAINED_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_PAGE_LINES: usize = 200;
const READ_BYTES: usize = 8_192;
// Complete a UTF-8 scalar crossing the publication boundary before truncating it.
const RAW_LINE_BYTES: usize = MAX_LINE_BYTES + 3;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConsoleStream {
    Stdout,
    Stderr,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConsoleError {
    InvalidPage,
    CursorExhausted,
    ReadFailed,
    Unavailable,
}
impl ConsoleError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::InvalidPage => "console_invalid_page",
            Self::CursorExhausted => "console_cursor_exhausted",
            Self::ReadFailed => "console_read_failed",
            Self::Unavailable => "console_unavailable",
        }
    }
}
impl fmt::Display for ConsoleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Console capture failed")
    }
}
impl std::error::Error for ConsoleError {}

/// Raw captured text. Debug is redacted, but this is not a sanitized task summary.
#[derive(Clone, Eq, PartialEq)]
pub struct ConsoleLine {
    pub cursor: u64,
    pub observed_at: SystemTime,
    pub stream: ConsoleStream,
    pub text: String,
    pub truncated: bool,
}
impl fmt::Debug for ConsoleLine {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ConsoleLine")
            .field("cursor", &self.cursor)
            .field("observed_at", &self.observed_at)
            .field("stream", &self.stream)
            .field("text_bytes", &self.text.len())
            .field("truncated", &self.truncated)
            .finish()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConsolePage {
    pub lines: Vec<ConsoleLine>,
    /// Last returned line, not the latest retained line: pagination cannot skip.
    pub next_cursor: u64,
    pub latest_cursor: u64,
    /// The requested prefix has been evicted. Returned lines are still ordered.
    pub gap: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CaptureReport {
    pub lines: u64,
    pub truncated_lines: u64,
}

struct Inner {
    lines: VecDeque<ConsoleLine>,
    text_bytes: usize,
    latest: u64,
}

/// Shared bounded storage. No listener registration or arbitrary stream labels.
#[derive(Clone)]
pub struct ConsoleCapture {
    inner: Arc<Mutex<Inner>>,
}
impl fmt::Debug for ConsoleCapture {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ConsoleCapture").finish_non_exhaustive()
    }
}
impl Default for ConsoleCapture {
    fn default() -> Self {
        Self::new()
    }
}
impl ConsoleCapture {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner {
                lines: VecDeque::new(),
                text_bytes: 0,
                latest: 0,
            })),
        }
    }

    /// Drain to EOF regardless of whether a viewer exists. LF, CR and CRLF are
    /// delimiters; empty lines are retained and a final unterminated line is flushed.
    /// Invalid UTF-8 is replaced explicitly, never interpreted as commands.
    ///
    /// A raw line retains at most MAX_LINE_BYTES + 3 even without a delimiter;
    /// the lookahead completes a boundary scalar. Excess bytes are drained/discarded
    /// until its delimiter. The published UTF-8
    /// text is also byte-bounded without splitting a scalar. Read failure is fixed
    /// and redacted; an incomplete line on failed I/O is not presented as complete.
    /// Reader calls happen outside the storage mutex. Snapshot copying holds that
    /// mutex for a bounded page; no hard capture latency/cancellation bound is claimed.
    pub fn capture_reader(
        &self,
        stream: ConsoleStream,
        mut reader: impl Read,
    ) -> Result<CaptureReport, ConsoleError> {
        let mut framer = Framer::new();
        let mut chunk = [0; READ_BYTES];
        let mut report = CaptureReport {
            lines: 0,
            truncated_lines: 0,
        };
        loop {
            let read = match reader.read(&mut chunk) {
                Ok(0) => break,
                Ok(read) => read,
                Err(error) if error.kind() == ErrorKind::Interrupted => continue,
                Err(_) => return Err(ConsoleError::ReadFailed),
            };
            for &byte in &chunk[..read] {
                if framer.suppress_lf {
                    framer.suppress_lf = false;
                    if byte == b'\n' {
                        continue;
                    }
                }
                match byte {
                    b'\r' | b'\n' => {
                        self.publish(stream, &mut framer, &mut report)?;
                        framer.suppress_lf = byte == b'\r';
                    }
                    _ => framer.push(byte),
                }
            }
        }
        if !framer.bytes.is_empty() || framer.truncated {
            self.publish(stream, &mut framer, &mut report)?;
        }
        Ok(report)
    }

    pub fn snapshot(&self, after: u64, limit: usize) -> Result<ConsolePage, ConsoleError> {
        if !(1..=MAX_PAGE_LINES).contains(&limit) {
            return Err(ConsoleError::InvalidPage);
        }
        let inner = self.inner.lock().map_err(|_| ConsoleError::Unavailable)?;
        if after > inner.latest {
            return Err(ConsoleError::InvalidPage);
        }
        let gap = inner
            .lines
            .front()
            .is_some_and(|line| after < line.cursor - 1);
        let lines: Vec<_> = inner
            .lines
            .iter()
            .filter(|line| line.cursor > after)
            .take(limit)
            .cloned()
            .collect();
        let next_cursor = lines.last().map_or(after, |line| line.cursor);
        Ok(ConsolePage {
            lines,
            next_cursor,
            latest_cursor: inner.latest,
            gap,
        })
    }

    fn publish(
        &self,
        stream: ConsoleStream,
        framer: &mut Framer,
        report: &mut CaptureReport,
    ) -> Result<(), ConsoleError> {
        let decoded = String::from_utf8_lossy(&framer.bytes);
        let mut end = decoded.len().min(MAX_LINE_BYTES);
        while !decoded.is_char_boundary(end) {
            end -= 1;
        }
        let truncated = framer.truncated || end < decoded.len();
        let text = decoded[..end].to_owned();
        let mut inner = self.inner.lock().map_err(|_| ConsoleError::Unavailable)?;
        let cursor = inner
            .latest
            .checked_add(1)
            .ok_or(ConsoleError::CursorExhausted)?;
        let next_lines = report
            .lines
            .checked_add(1)
            .ok_or(ConsoleError::CursorExhausted)?;
        let next_truncated = report
            .truncated_lines
            .checked_add(u64::from(truncated))
            .ok_or(ConsoleError::CursorExhausted)?;
        while inner.lines.len() >= MAX_RETAINED_LINES
            || inner.text_bytes + text.len() > MAX_RETAINED_BYTES
        {
            let removed = inner.lines.pop_front().ok_or(ConsoleError::Unavailable)?;
            inner.text_bytes -= removed.text.len();
        }
        inner.text_bytes += text.len();
        inner.lines.push_back(ConsoleLine {
            cursor,
            observed_at: SystemTime::now(),
            stream,
            text,
            truncated,
        });
        inner.latest = cursor;
        report.lines = next_lines;
        report.truncated_lines = next_truncated;
        framer.bytes.clear();
        framer.truncated = false;
        Ok(())
    }
}

struct Framer {
    bytes: Vec<u8>,
    truncated: bool,
    suppress_lf: bool,
}
impl Framer {
    fn push(&mut self, byte: u8) {
        if self.bytes.len() < RAW_LINE_BYTES {
            self.bytes.push(byte);
        } else {
            self.truncated = true;
        }
    }

    fn new() -> Self {
        Self {
            bytes: Vec::with_capacity(RAW_LINE_BYTES),
            truncated: false,
            suppress_lf: false,
        }
    }
}

#[cfg(test)]
mod tests;
