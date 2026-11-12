//! Transport-neutral bounded length-prefixed UTF-8 framing.
//!
//! This is a stateless byte-framing foundation and nothing more: it turns bytes into a
//! bounded UTF-8 string and back. It contains no JSON, no message DTOs, no type
//! registry, no routing, no identity, no authorization and no session state. Possessing
//! a frame, or reading one successfully, confers no management authority whatsoever.
//!
//! It opens no socket, starts no thread and sets no deadline. A caller supplies its own
//! reader and writer, so timeouts, authentication and policy all remain the caller's.
//!
//! # Stream contract
//!
//! Framing is positional. There is no resynchronization: once a prefix or body has
//! begun, its bytes are fixed, and a partial read cannot be retried against a new
//! frame. **Any error requires the caller to discard the stream.** The bytes after a
//! failed frame are at an unknown offset and cannot be trusted, so recovering from a
//! truncation or a rejected length in place is not offered. This applies to write
//! failures too: a half-written frame is not resumable, and nothing here keeps the
//! stream usable after an error.
//!
//! # Deliberate differences from the Java codecs
//!
//! Neither Java reader can distinguish a clean close from a truncated frame, and the
//! two disagree about where they stop. `TcpMessageCodec.readExactly` returns `null` for
//! *any* short read, so a prefix cut off part-way is reported as an ordinary end of
//! stream. `ControlCodec.readInt` propagates its EOF instead, and does so even on a
//! completely clean close before any byte arrives. Both are replaced here rather than
//! reproduced: `Ok(None)` is returned only for a close *before the first byte*, and any
//! other short read is an error. See [`FrameError::TruncatedPrefix`].
//!
//! Zero-length frames differ as well. `TcpMessageCodec` admits a declared length of
//! zero and passes the empty body to its JSON parser, where it fails as a parse error
//! rather than a framing error. `ControlCodec` rejects zero outright. Here zero is
//! refused by the framing itself, before any body handling. See [`FrameError::EmptyBody`].

use std::fmt;
use std::io::{ErrorKind, Read, Write};

/// Largest accepted body for any frame, regardless of channel.
pub const MAX_FRAME_BYTES: usize = 16 * 1024 * 1024;
/// Largest accepted body for the wrapper/plugin control channel.
pub const MAX_CONTROL_FRAME_BYTES: usize = 1024 * 1024;

/// Why a frame was refused. Every variant is input-free and carries no diagnostics.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FrameError {
    /// A [`FrameLimit`] was constructed outside the usable range.
    InvalidLimit,
    /// A body was empty. An empty frame carries no message and is not "no message".
    EmptyBody,
    /// The declared body exceeded the active limit. Refused before any allocation.
    BodyTooLarge,
    /// The stream ended part-way through the four-byte length prefix.
    TruncatedPrefix,
    /// The stream ended part-way through the declared body.
    TruncatedBody,
    /// The body was not valid UTF-8.
    InvalidUtf8,
    /// The underlying reader or writer failed. The cause is deliberately discarded.
    Io,
}
impl FrameError {
    /// A stable, input-free identifier suitable for logs and error surfaces.
    pub const fn code(self) -> &'static str {
        match self {
            Self::InvalidLimit => "frame_invalid_limit",
            Self::EmptyBody => "frame_empty_body",
            Self::BodyTooLarge => "frame_body_too_large",
            Self::TruncatedPrefix => "frame_truncated_prefix",
            Self::TruncatedBody => "frame_truncated_body",
            Self::InvalidUtf8 => "frame_invalid_utf8",
            Self::Io => "frame_io_error",
        }
    }
}
impl fmt::Display for FrameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Framing failed: {}", self.code())
    }
}
impl std::error::Error for FrameError {}

/// The maximum body size one channel will accept, chosen once and reused.
///
/// A limit is validated at construction, so a caller cannot pass an unbounded or
/// zero budget to a frame operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FrameLimit {
    max_body_bytes: usize,
}
impl FrameLimit {
    /// Build a limit, refusing anything outside `1..=MAX_FRAME_BYTES`.
    pub fn new(max_body_bytes: usize) -> Result<Self, FrameError> {
        if max_body_bytes == 0 || max_body_bytes > MAX_FRAME_BYTES {
            return Err(FrameError::InvalidLimit);
        }
        Ok(Self { max_body_bytes })
    }
    /// The accepted body size, in bytes.
    pub fn max_body_bytes(&self) -> usize {
        self.max_body_bytes
    }
}

/// One decoded frame body.
///
/// The bytes are private and exposed read-only. `Debug` is redacted because a body is
/// peer-supplied text that may carry identifiers, and framing has no business logging it.
pub struct FrameBody {
    text: String,
}
impl FrameBody {
    pub fn as_str(&self) -> &str {
        &self.text
    }
    pub fn as_bytes(&self) -> &[u8] {
        self.text.as_bytes()
    }
}
impl fmt::Debug for FrameBody {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FrameBody")
            .field("len", &self.text.len())
            .finish_non_exhaustive()
    }
}

/// Read one frame from `reader`.
///
/// `Ok(None)` means the stream ended cleanly before any byte of a frame was read. Any
/// other short read, an unusable declared length, or an undecodable body is an error,
/// and the caller must discard the stream.
pub fn read_frame<R: Read + ?Sized>(
    reader: &mut R,
    limit: FrameLimit,
) -> Result<Option<FrameBody>, FrameError> {
    let Some(prefix) = read_prefix(reader)? else {
        return Ok(None);
    };

    // Check before allocating: a prefix alone can request at most the admitted
    // per-frame budget, never an unchecked peer-selected allocation.
    let length = u32::from_be_bytes(prefix) as usize;
    if length == 0 {
        return Err(FrameError::EmptyBody);
    }
    if length > limit.max_body_bytes() {
        return Err(FrameError::BodyTooLarge);
    }

    let mut body = vec![0u8; length];
    read_body(reader, &mut body)?;
    let text = String::from_utf8(body).map_err(|_| FrameError::InvalidUtf8)?;
    Ok(Some(FrameBody { text }))
}

/// Write one frame to `writer`.
///
/// The body is checked against the limit before the first byte is written. This does not
/// flush: buffering and flush policy belong to the caller.
pub fn write_frame<W: Write + ?Sized>(
    writer: &mut W,
    body: &str,
    limit: FrameLimit,
) -> Result<(), FrameError> {
    if body.is_empty() {
        return Err(FrameError::EmptyBody);
    }
    if body.len() > limit.max_body_bytes() {
        return Err(FrameError::BodyTooLarge);
    }

    // `MAX_FRAME_BYTES` is below `u32::MAX`, so this cannot fail for any body a valid
    // `FrameLimit` accepts. It is kept explicit so that changing a constant cannot
    // silently truncate a declared length.
    let length = u32::try_from(body.len()).map_err(|_| FrameError::BodyTooLarge)?;
    write_all(writer, &length.to_be_bytes())?;
    write_all(writer, body.as_bytes())
}

/// Fill the four-byte prefix, or report a clean close that happened before any byte.
///
/// A close part-way through the prefix is `TruncatedPrefix`, not a clean end: the peer
/// sent something and then stopped, which is not the same event as never sending.
fn read_prefix<R: Read + ?Sized>(reader: &mut R) -> Result<Option<[u8; 4]>, FrameError> {
    let mut prefix = [0u8; 4];
    let mut filled = 0usize;
    while filled < prefix.len() {
        match read_once(reader, &mut prefix[filled..])? {
            0 if filled == 0 => return Ok(None),
            0 => return Err(FrameError::TruncatedPrefix),
            read => filled += read,
        }
    }
    Ok(Some(prefix))
}

/// Fill the body buffer, or report that it ended early.
fn read_body<R: Read + ?Sized>(reader: &mut R, body: &mut [u8]) -> Result<(), FrameError> {
    let mut filled = 0usize;
    while filled < body.len() {
        match read_once(reader, &mut body[filled..])? {
            0 => return Err(FrameError::TruncatedBody),
            read => filled += read,
        }
    }
    Ok(())
}

/// One read attempt, retrying an interrupt instead of failing the frame.
///
/// An interrupt is not an error condition for a stream read, so it is retried rather
/// than surfaced; the caller still owns any deadline.
fn read_once<R: Read + ?Sized>(reader: &mut R, into: &mut [u8]) -> Result<usize, FrameError> {
    loop {
        match reader.read(into) {
            Ok(read) => return Ok(read),
            Err(error) if error.kind() == ErrorKind::Interrupted => continue,
            // The underlying cause is deliberately not carried: it may quote peer data.
            Err(_) => return Err(FrameError::Io),
        }
    }
}

/// Write every byte, retrying an interrupt.
fn write_all<W: Write + ?Sized>(writer: &mut W, bytes: &[u8]) -> Result<(), FrameError> {
    writer.write_all(bytes).map_err(|_| FrameError::Io)
}

#[cfg(test)]
#[path = "framing_tests.rs"]
mod tests;
