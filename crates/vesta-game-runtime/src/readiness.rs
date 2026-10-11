//! Bounded Minecraft readiness evidence and a synchronous Server List Ping probe.
//!
//! A live child or a TCP connection is not world-ready evidence. Normally a server
//! must log `Done (<decimal>s)! For help, type "help"` (optionally followed by
//! ` or "?"`) and then answer a status probe. With no probe configured this is
//! explicitly log-only readiness. A console gap before readiness is the one
//! exception: a successful status response can replace the missing done line.
//!
//! Accepted log prefixes are exactly `[HH:MM:SS] [Server thread/INFO]: `,
//! `[HH:MM:SS] [Server thread/INFO] [logger]: `, and `[HH:MM:SS INFO]: `.
//! Bare done lines are also accepted, but bare fatal lines are not. Fatal lines
//! additionally accept WARN and ERROR in the same prefix forms. Logger names
//! are nonempty ASCII alphanumeric names with `/`, `.`, `_`, `-`, or `$`.
//! Timestamps must be valid 24-hour times. No substring search, chat prefixes,
//! trailing text (except the historical help suffix), or other thread/level is
//! accepted. This is format filtering, not cryptographic log authentication.
//!
//! Callers own process-liveness tracking and must serialize generation events,
//! console events, and completed probes; discard results from older generations.
//! Run `status_probe` off the control/console thread. `state` is pure and never
//! performs I/O. Status probing verifies the configured endpoint, not its process
//! identity; callers must separately establish successful bind ownership.

use std::io::{self, Read, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpStream};
use std::time::{Duration, Instant};

const MAX_INPUT: usize = 64 * 1024;
const MAX_JSON_DEPTH: usize = 32;
const FAILURE_THRESHOLD: u8 = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Readiness {
    Stopped,
    Starting,
    Ready,
    Degraded(DegradedReason),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DegradedReason {
    StartupTimeout,
    ProbeFailed,
    FatalOutput,
    ConsoleGap,
}

#[derive(Debug, Clone)]
pub struct ReadinessConfig {
    pub probe: Option<SocketAddr>,
    pub startup_timeout: Duration,
    pub probe_interval: Duration,
    pub probe_timeout: Duration,
}

impl Default for ReadinessConfig {
    fn default() -> Self {
        Self {
            probe: None,
            startup_timeout: Duration::from_secs(300),
            probe_interval: Duration::from_secs(15),
            probe_timeout: Duration::from_secs(3),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ReadinessTracker {
    config: ReadinessConfig,
    started: Option<Instant>,
    done: bool,
    confirmed: bool,
    gap: bool,
    fatal: bool,
    failures: u8,
    last_probe: Option<Instant>,
}

impl ReadinessTracker {
    pub fn new(config: ReadinessConfig) -> Self {
        Self {
            config,
            started: None,
            done: false,
            confirmed: false,
            gap: false,
            fatal: false,
            failures: 0,
            last_probe: None,
        }
    }

    pub fn generation_started(&mut self, now: Instant) {
        self.generation_stopped();
        self.started = Some(now);
    }

    pub fn generation_stopped(&mut self) {
        self.started = None;
        self.done = false;
        self.confirmed = false;
        self.gap = false;
        self.fatal = false;
        self.failures = 0;
        self.last_probe = None;
    }

    pub fn observe_line(&mut self, text: &str) {
        if self.started.is_none() || text.len() > MAX_INPUT {
            return;
        }
        let (message, level) = match log_message(text) {
            Some((message, level)) => (message, Some(level)),
            None => (text, None),
        };
        if level.is_none_or(|level| level == "INFO") && is_done(message) {
            if !self.done {
                self.done = true;
                // Earlier probe successes cannot confirm this done event.
                if !self.gap && self.config.probe.is_none() {
                    self.confirmed = true;
                }
            }
        } else if level.is_some()
            && (!self.confirmed || self.failures >= FAILURE_THRESHOLD)
            && is_fatal(message)
        {
            self.fatal = true;
        }
    }

    /// Lost startup output invalidates log evidence until a successful probe.
    /// Without a configured probe, a gap cannot be cleared in this generation.
    pub fn observe_gap(&mut self) {
        if self.started.is_some() && !self.confirmed {
            self.gap = true;
        }
    }

    /// Due immediately after generation start, then at the configured interval
    /// from the last recorded completion (including failures). At most one probe
    /// should be in flight; polling this pure method does not reserve a probe.
    pub fn probe_due(&self, now: Instant) -> Option<SocketAddr> {
        self.started?;
        let address = self.config.probe?;
        if self.last_probe.is_none_or(|last| {
            now.checked_duration_since(last)
                .is_some_and(|elapsed| elapsed >= self.config.probe_interval)
        }) {
            Some(address)
        } else {
            None
        }
    }

    pub fn record_probe(&mut self, now: Instant, result: Result<StatusSummary, ProbeError>) {
        if self.started.is_none() || self.config.probe.is_none() {
            return;
        }
        self.last_probe = Some(now);
        match result {
            Ok(_) => {
                self.failures = 0;
                if self.done || self.gap {
                    self.confirmed = true;
                    self.gap = false;
                }
            }
            Err(_) => self.failures = self.failures.saturating_add(1),
        }
    }

    /// Precedence is stopped, sticky fatal, unresolved gap, confirmed readiness
    /// (or three probe failures), startup timeout, then starting. Timeout uses
    /// `>=`; an injected instant before generation start does not time out.
    pub fn state(&self, now: Instant) -> Readiness {
        let Some(started) = self.started else {
            return Readiness::Stopped;
        };
        if self.fatal {
            return Readiness::Degraded(DegradedReason::FatalOutput);
        }
        if self.gap {
            return Readiness::Degraded(DegradedReason::ConsoleGap);
        }
        if self.confirmed {
            return if self.failures >= FAILURE_THRESHOLD {
                Readiness::Degraded(DegradedReason::ProbeFailed)
            } else {
                Readiness::Ready
            };
        }
        if now
            .checked_duration_since(started)
            .is_some_and(|elapsed| elapsed >= self.config.startup_timeout)
        {
            return Readiness::Degraded(DegradedReason::StartupTimeout);
        }
        Readiness::Starting
    }

    pub fn probe_configured(&self) -> bool {
        self.config.probe.is_some()
    }
}

fn timestamp(text: &str) -> bool {
    let bytes = text.as_bytes();
    bytes.len() == 8
        && bytes[2] == b':'
        && bytes[5] == b':'
        && [0, 1, 3, 4, 6, 7]
            .iter()
            .all(|&i| bytes[i].is_ascii_digit())
        && &text[..2] <= "23"
        && &text[3..5] <= "59"
        && &text[6..8] <= "59"
}

fn log_message(text: &str) -> Option<(&str, &str)> {
    let rest = text.strip_prefix('[')?;
    let end = rest.find(']')?;
    let head = &rest[..end];
    let rest = &rest[end + 1..];
    if let Some((time, level)) = head.split_once(' ') {
        if timestamp(time) && matches!(level, "INFO" | "WARN" | "ERROR") {
            return rest.strip_prefix(": ").map(|message| (message, level));
        }
        return None;
    }
    if !timestamp(head) {
        return None;
    }
    let rest = rest.strip_prefix(" [Server thread/")?;
    let end = rest.find(']')?;
    let level = &rest[..end];
    if !matches!(level, "INFO" | "WARN" | "ERROR") {
        return None;
    }
    let rest = &rest[end + 1..];
    if let Some(message) = rest.strip_prefix(": ") {
        return Some((message, level));
    }
    let rest = rest.strip_prefix(" [")?;
    let end = rest.find(']')?;
    let logger = &rest[..end];
    if logger.is_empty()
        || !logger
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"/._-$".contains(&b))
    {
        return None;
    }
    rest[end + 1..]
        .strip_prefix(": ")
        .map(|message| (message, level))
}

fn is_done(message: &str) -> bool {
    let Some(rest) = message.strip_prefix("Done (") else {
        return false;
    };
    let Some((seconds, help)) = rest.split_once("s)! For help, type ") else {
        return false;
    };
    let mut decimal = false;
    let mut digits = 0;
    for b in seconds.bytes() {
        if b.is_ascii_digit() {
            digits += 1;
        } else if b == b'.' && !decimal && digits > 0 {
            decimal = true;
            digits = 0;
        } else {
            return false;
        }
    }
    digits > 0 && matches!(help, "\"help\"" | "\"help\" or \"?\"")
}

fn is_fatal(message: &str) -> bool {
    message == "**** FAILED TO BIND TO PORT!"
        || message == "Failed to start the minecraft server"
        || message == "Encountered an unexpected exception"
        || message.starts_with("This crash report has been saved to:")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StatusSummary {
    pub online: u32,
    pub max: u32,
    pub protocol: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeError {
    Connect,
    Timeout,
    Protocol,
    TooLarge,
}

/// Resolve only literal server properties, never DNS or SRV. Missing/empty bind
/// addresses use IPv4 loopback; wildcard IPv6 uses IPv6 loopback. Last duplicate
/// key wins, matching ordinary properties usage. Invalid relevant values fail
/// closed. Properties larger than 64 KiB are rejected; Java escapes are omitted.
pub fn probe_address(server_properties: &str) -> Option<SocketAddr> {
    if server_properties.len() > MAX_INPUT {
        return None;
    }
    let mut ip = "";
    let mut port = "25565";
    for line in server_properties.lines() {
        let line = line.trim();
        if line.starts_with('#') || line.starts_with('!') || line.is_empty() {
            continue;
        }
        if let Some((key, value)) = line.split_once('=') {
            match key.trim() {
                "server-ip" => ip = value.trim(),
                "server-port" => port = value.trim(),
                _ => {}
            }
        }
    }
    let port: u16 = port.parse().ok()?;
    if port == 0 {
        return None;
    }
    let ip = if ip.is_empty() {
        IpAddr::V4(Ipv4Addr::LOCALHOST)
    } else {
        match ip.parse::<IpAddr>().ok()? {
            IpAddr::V4(ip) if ip.is_unspecified() => IpAddr::V4(Ipv4Addr::LOCALHOST),
            IpAddr::V6(ip) if ip.is_unspecified() => IpAddr::V6(Ipv6Addr::LOCALHOST),
            ip => ip,
        }
    };
    Some(SocketAddr::new(ip, port))
}

/// Modern (1.7+) Minecraft Server List Ping, with one overall connect/write/read
/// deadline and a total response budget of 64 KiB including the frame prefix.
/// Returns only numeric fields, never untrusted MOTD/version text. This checks a
/// status response, not actual player login or bind ownership. No redirects/SRV.
pub fn status_probe(addr: SocketAddr, timeout: Duration) -> Result<StatusSummary, ProbeError> {
    let deadline = Instant::now()
        .checked_add(timeout)
        .ok_or(ProbeError::Timeout)?;
    let mut stream = TcpStream::connect_timeout(&addr, remaining(deadline)?)
        .map_err(|error| io_error(error, ProbeError::Connect))?;
    let host = addr.ip().to_string();
    let mut handshake = Vec::with_capacity(64);
    varint_push(&mut handshake, 0); // Handshake packet ID.
    varint_push(&mut handshake, 47); // Version-independent status negotiation.
    varint_push(&mut handshake, host.len() as u32);
    handshake.extend_from_slice(host.as_bytes());
    handshake.extend_from_slice(&addr.port().to_be_bytes());
    varint_push(&mut handshake, 1); // Next state: status.
    let mut request = Vec::with_capacity(70);
    varint_push(&mut request, handshake.len() as u32);
    request.extend_from_slice(&handshake);
    request.extend_from_slice(&[1, 0]); // Status request frame and ID.
    let mut unwritten = request.as_slice();
    while !unwritten.is_empty() {
        stream
            .set_write_timeout(Some(remaining(deadline)?))
            .map_err(|error| io_error(error, ProbeError::Protocol))?;
        match stream.write(unwritten) {
            Ok(0) => return Err(ProbeError::Protocol),
            Ok(count) => unwritten = &unwritten[count..],
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(io_error(error, ProbeError::Protocol)),
        }
    }
    let mut prefix = [0; 5];
    let mut prefix_len = 0;
    let length = loop {
        read_deadline(
            &mut stream,
            &mut prefix[prefix_len..prefix_len + 1],
            deadline,
        )?;
        prefix_len += 1;
        if prefix[prefix_len - 1] & 0x80 == 0 {
            let mut cursor = 0;
            break varint_read(&prefix[..prefix_len], &mut cursor)? as usize;
        }
        if prefix_len == 5 {
            return Err(ProbeError::Protocol);
        }
    };
    if length > MAX_INPUT - prefix_len {
        return Err(ProbeError::TooLarge);
    }
    let mut packet = vec![0; length];
    read_deadline(&mut stream, &mut packet, deadline)?;
    let mut cursor = 0;
    if varint_read(&packet, &mut cursor)? != 0 {
        return Err(ProbeError::Protocol);
    }
    let json_length = varint_read(&packet, &mut cursor)? as usize;
    if json_length != packet.len() - cursor {
        return Err(ProbeError::Protocol);
    }
    let summary = JsonParser::parse(&packet[cursor..])?;
    remaining(deadline)?;
    Ok(summary)
}

fn remaining(deadline: Instant) -> Result<Duration, ProbeError> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|duration| !duration.is_zero())
        .ok_or(ProbeError::Timeout)
}

fn io_error(error: io::Error, fallback: ProbeError) -> ProbeError {
    if matches!(
        error.kind(),
        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
    ) {
        ProbeError::Timeout
    } else {
        fallback
    }
}

fn read_deadline(
    stream: &mut TcpStream,
    mut buffer: &mut [u8],
    deadline: Instant,
) -> Result<(), ProbeError> {
    while !buffer.is_empty() {
        stream
            .set_read_timeout(Some(remaining(deadline)?))
            .map_err(|error| io_error(error, ProbeError::Protocol))?;
        match stream.read(buffer) {
            Ok(0) => return Err(ProbeError::Protocol),
            Ok(count) => buffer = &mut buffer[count..],
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(io_error(error, ProbeError::Protocol)),
        }
    }
    Ok(())
}

fn varint_push(buffer: &mut Vec<u8>, mut number: u32) {
    loop {
        let byte = (number & 0x7f) as u8;
        number >>= 7;
        buffer.push(byte | if number != 0 { 0x80 } else { 0 });
        if number == 0 {
            break;
        }
    }
}

fn varint_read(bytes: &[u8], cursor: &mut usize) -> Result<u32, ProbeError> {
    let mut value = 0;
    for index in 0..5 {
        let byte = *bytes.get(*cursor).ok_or(ProbeError::Protocol)?;
        *cursor += 1;
        if index == 4 && byte & 0xf0 != 0 {
            return Err(ProbeError::Protocol);
        }
        value |= u32::from(byte & 0x7f) << (index * 7);
        if byte & 0x80 == 0 {
            return Ok(value);
        }
    }
    Err(ProbeError::Protocol)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ObjectKind {
    Root,
    Players,
    Version,
    Other,
}

// A full bounded JSON syntax validator with narrowly scoped numeric extraction.
// Unknown values are traversed, not trusted or retained; nesting is capped at 32.
// Relevant duplicate keys, wrong types, missing fields, noninteger/range-invalid
// counters and protocol values are rejected. Keys may use valid JSON escapes.
struct JsonParser<'a> {
    bytes: &'a [u8],
    cursor: usize,
    online: Option<u32>,
    max: Option<u32>,
    protocol: Option<i32>,
    players_seen: bool,
    version_seen: bool,
}

impl<'a> JsonParser<'a> {
    fn parse(bytes: &'a [u8]) -> Result<StatusSummary, ProbeError> {
        if bytes.len() > MAX_INPUT {
            return Err(ProbeError::TooLarge);
        }
        std::str::from_utf8(bytes).map_err(|_| ProbeError::Protocol)?;
        let mut parser = Self {
            bytes,
            cursor: 0,
            online: None,
            max: None,
            protocol: None,
            players_seen: false,
            version_seen: false,
        };
        parser.space();
        parser.object(ObjectKind::Root, 0)?;
        parser.space();
        if parser.cursor != bytes.len() {
            return Err(ProbeError::Protocol);
        }
        Ok(StatusSummary {
            online: parser.online.ok_or(ProbeError::Protocol)?,
            max: parser.max.ok_or(ProbeError::Protocol)?,
            protocol: parser.protocol.ok_or(ProbeError::Protocol)?,
        })
    }

    fn space(&mut self) {
        while self
            .bytes
            .get(self.cursor)
            .is_some_and(|b| b" \r\n\t".contains(b))
        {
            self.cursor += 1;
        }
    }

    fn take(&mut self, expected: u8) -> Result<(), ProbeError> {
        self.space();
        if self.bytes.get(self.cursor) != Some(&expected) {
            return Err(ProbeError::Protocol);
        }
        self.cursor += 1;
        Ok(())
    }

    fn object(&mut self, kind: ObjectKind, depth: usize) -> Result<(), ProbeError> {
        if depth >= MAX_JSON_DEPTH {
            return Err(ProbeError::Protocol);
        }
        self.take(b'{')?;
        self.space();
        if self.bytes.get(self.cursor) == Some(&b'}') {
            self.cursor += 1;
            return Ok(());
        }
        loop {
            let key = self.string()?;
            self.take(b':')?;
            self.space();
            match (kind, key.as_str()) {
                (ObjectKind::Root, "players") => {
                    if self.players_seen {
                        return Err(ProbeError::Protocol);
                    }
                    self.players_seen = true;
                    self.object(ObjectKind::Players, depth + 1)?;
                }
                (ObjectKind::Root, "version") => {
                    if self.version_seen {
                        return Err(ProbeError::Protocol);
                    }
                    self.version_seen = true;
                    self.object(ObjectKind::Version, depth + 1)?;
                }
                (ObjectKind::Players, "online") => {
                    if self.online.is_some() {
                        return Err(ProbeError::Protocol);
                    }
                    self.online = Some(self.number()?.parse().map_err(|_| ProbeError::Protocol)?);
                }
                (ObjectKind::Players, "max") => {
                    if self.max.is_some() {
                        return Err(ProbeError::Protocol);
                    }
                    self.max = Some(self.number()?.parse().map_err(|_| ProbeError::Protocol)?);
                }
                (ObjectKind::Version, "protocol") => {
                    if self.protocol.is_some() {
                        return Err(ProbeError::Protocol);
                    }
                    self.protocol = Some(self.number()?.parse().map_err(|_| ProbeError::Protocol)?);
                }
                _ => self.value(depth + 1)?,
            }
            self.space();
            match self.bytes.get(self.cursor) {
                Some(b'}') => {
                    self.cursor += 1;
                    return Ok(());
                }
                Some(b',') => {
                    self.cursor += 1;
                }
                _ => return Err(ProbeError::Protocol),
            }
        }
    }

    fn value(&mut self, depth: usize) -> Result<(), ProbeError> {
        if depth >= MAX_JSON_DEPTH {
            return Err(ProbeError::Protocol);
        }
        self.space();
        match self.bytes.get(self.cursor) {
            Some(b'{') => self.object(ObjectKind::Other, depth),
            Some(b'[') => {
                self.cursor += 1;
                self.space();
                if self.bytes.get(self.cursor) == Some(&b']') {
                    self.cursor += 1;
                    return Ok(());
                }
                loop {
                    self.value(depth + 1)?;
                    self.space();
                    match self.bytes.get(self.cursor) {
                        Some(b']') => {
                            self.cursor += 1;
                            return Ok(());
                        }
                        Some(b',') => {
                            self.cursor += 1;
                        }
                        _ => return Err(ProbeError::Protocol),
                    }
                }
            }
            Some(b'"') => self.string().map(|_| ()),
            Some(b't') => self.literal(b"true"),
            Some(b'f') => self.literal(b"false"),
            Some(b'n') => self.literal(b"null"),
            Some(b'-' | b'0'..=b'9') => self.number().map(|_| ()),
            _ => Err(ProbeError::Protocol),
        }
    }

    fn literal(&mut self, literal: &[u8]) -> Result<(), ProbeError> {
        if self.bytes.get(self.cursor..self.cursor + literal.len()) != Some(literal) {
            return Err(ProbeError::Protocol);
        }
        self.cursor += literal.len();
        Ok(())
    }

    fn number(&mut self) -> Result<&'a str, ProbeError> {
        self.space();
        let start = self.cursor;
        if self.bytes.get(self.cursor) == Some(&b'-') {
            self.cursor += 1;
        }
        match self.bytes.get(self.cursor) {
            Some(b'0') => self.cursor += 1,
            Some(b'1'..=b'9') => {
                self.cursor += 1;
                while self.bytes.get(self.cursor).is_some_and(u8::is_ascii_digit) {
                    self.cursor += 1;
                }
            }
            _ => return Err(ProbeError::Protocol),
        }
        if self.bytes.get(self.cursor) == Some(&b'.') {
            self.cursor += 1;
            self.digits()?;
        }
        if matches!(self.bytes.get(self.cursor), Some(b'e' | b'E')) {
            self.cursor += 1;
            if matches!(self.bytes.get(self.cursor), Some(b'+' | b'-')) {
                self.cursor += 1;
            }
            self.digits()?;
        }
        std::str::from_utf8(&self.bytes[start..self.cursor]).map_err(|_| ProbeError::Protocol)
    }

    fn digits(&mut self) -> Result<(), ProbeError> {
        let start = self.cursor;
        while self.bytes.get(self.cursor).is_some_and(u8::is_ascii_digit) {
            self.cursor += 1;
        }
        if start == self.cursor {
            return Err(ProbeError::Protocol);
        }
        Ok(())
    }

    fn string(&mut self) -> Result<String, ProbeError> {
        self.take(b'"')?;
        let mut output = Vec::new();
        loop {
            let byte = *self.bytes.get(self.cursor).ok_or(ProbeError::Protocol)?;
            self.cursor += 1;
            match byte {
                b'"' => return String::from_utf8(output).map_err(|_| ProbeError::Protocol),
                b'\\' => {
                    let escape = *self.bytes.get(self.cursor).ok_or(ProbeError::Protocol)?;
                    self.cursor += 1;
                    match escape {
                        b'"' | b'\\' | b'/' => output.push(escape),
                        b'b' => output.push(8),
                        b'f' => output.push(12),
                        b'n' => output.push(b'\n'),
                        b'r' => output.push(b'\r'),
                        b't' => output.push(b'\t'),
                        b'u' => {
                            let first = self.hex_quad()?;
                            let code = if (0xd800..=0xdbff).contains(&first) {
                                if self.bytes.get(self.cursor..self.cursor + 2) != Some(b"\\u") {
                                    return Err(ProbeError::Protocol);
                                }
                                self.cursor += 2;
                                let second = self.hex_quad()?;
                                if !(0xdc00..=0xdfff).contains(&second) {
                                    return Err(ProbeError::Protocol);
                                }
                                0x10000 + ((first - 0xd800) << 10) + second - 0xdc00
                            } else {
                                first
                            };
                            let character = char::from_u32(code).ok_or(ProbeError::Protocol)?;
                            let mut buffer = [0; 4];
                            output.extend_from_slice(character.encode_utf8(&mut buffer).as_bytes());
                        }
                        _ => return Err(ProbeError::Protocol),
                    }
                }
                0..=31 => return Err(ProbeError::Protocol),
                byte => output.push(byte),
            }
        }
    }

    fn hex_quad(&mut self) -> Result<u32, ProbeError> {
        let bytes = self
            .bytes
            .get(self.cursor..self.cursor + 4)
            .ok_or(ProbeError::Protocol)?;
        self.cursor += 4;
        let mut result = 0;
        for &byte in bytes {
            result = (result << 4) | (byte as char).to_digit(16).ok_or(ProbeError::Protocol)?;
        }
        Ok(result)
    }
}

#[cfg(test)]
#[path = "readiness_tests.rs"]
mod tests;
