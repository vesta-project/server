//! Bounded descriptive parsing of `list` command console output, not authority.
//!
//! This is a pure function over one console line. It performs no process launch, no
//! query, no filesystem access, no network access and no authorization: it cannot
//! decide who may be seen, only what a recognized line describes. A caller that
//! exposes the result must still enforce current console-read policy.
//!
//! It is not the Java `PlayersQuery`: nothing here issues `list`, polls a capture,
//! waits for a deadline, or reports readiness. It is not Java text emulation: the
//! grammar is implemented directly, and recognized-but-unusable input is refused
//! rather than repaired into a plausible-looking zero-player answer.
//!
//! A line that is not a player list yields `NotRecognized`, which is deliberately
//! distinct from a line that *is* a player list but cannot be trusted.
use std::fmt;
use std::iter::Peekable;

/// Largest accepted input, in UTF-8 bytes, measured before any other work.
pub const MAX_INPUT_BYTES: usize = 256 * 1024;
/// Largest accepted number of comma-separated tokens in the names tail.
///
/// This bounds how much work one line can cause, not how large a server may be. It
/// counts tokens, including any later dropped as a bare UUID. It is deliberately not
/// a statement about the declared `max`: a configured capacity has no bearing on the
/// length of one console line, and operator-set overflow slots can make an announced
/// capacity exceed any sane entry count. Counts are descriptions, not proof that a
/// server is up, configured, or who is entitled to read them.
pub const MAX_PLAYERS: usize = 16_384;
/// Largest accepted single player name, in UTF-8 bytes.
pub const MAX_NAME_BYTES: usize = 64;

/// Why a line did not produce a player list. Every variant is input-free.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PlayerListError {
    /// The line is not a player list. It says nothing about players either way.
    NotRecognized,
    /// The line has a player-list shape but an unusable body: an absent grammar
    /// separator, a dangling or unrecognized format escape, an empty name token, or
    /// an oversized or control-bearing name. A count that contradicts the listed
    /// names is [`Inconsistent`](Self::Inconsistent), not malformed.
    Malformed,
    /// The line exceeds [`MAX_INPUT_BYTES`].
    TooLarge,
    /// The line lists more tokens than [`MAX_PLAYERS`].
    TooManyPlayers,
    /// A count field is longer than any real capacity or does not fit the type.
    NumberOverflow,
    /// The declared count and the listed names cannot both be true.
    Inconsistent,
}
impl PlayerListError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::NotRecognized => "players_not_recognized",
            Self::Malformed => "players_malformed_list",
            Self::TooLarge => "players_input_too_large",
            Self::TooManyPlayers => "players_too_many_players",
            Self::NumberOverflow => "players_number_overflow",
            Self::Inconsistent => "players_inconsistent_list",
        }
    }
}
impl fmt::Display for PlayerListError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Player list parsing failed")
    }
}
impl std::error::Error for PlayerListError {}

type Result<T> = std::result::Result<T, PlayerListError>;

/// One listed player. The name and UUID are private and exposed read-only.
/// `Debug` is redacted: player names are console-derived text, not safe to log.
#[derive(Clone, Eq, PartialEq)]
pub struct PlayerEntry {
    name: String,
    uuid: Option<String>,
}
impl PlayerEntry {
    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn uuid(&self) -> Option<&str> {
        self.uuid.as_deref()
    }
}
impl fmt::Debug for PlayerEntry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PlayerEntry")
            .field("name", &"[redacted]")
            .field("has_uuid", &self.uuid.is_some())
            .finish_non_exhaustive()
    }
}

/// A recognized player list. Counts are exactly what the line stated; they are never
/// derived from the number of names, because a stated count and a visible name list
/// disagreeing is a reason to refuse, not to guess.
pub struct PlayerList {
    online: u32,
    max: u32,
    players: Vec<PlayerEntry>,
}
impl PlayerList {
    pub fn online(&self) -> u32 {
        self.online
    }
    pub fn max(&self) -> u32 {
        self.max
    }
    pub fn players(&self) -> &[PlayerEntry] {
        &self.players
    }
}
impl fmt::Debug for PlayerList {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PlayerList")
            .field("online", &self.online)
            .field("max", &self.max)
            .field("player_count", &self.players.len())
            .finish_non_exhaustive()
    }
}

/// Parse one console line. `Ok` requires a fully understood, self-consistent list.
pub fn parse_player_list_line(line: &str) -> Result<PlayerList> {
    if line.len() > MAX_INPUT_BYTES {
        return Err(PlayerListError::TooLarge);
    }
    let stripped = strip_formatting(line)?;
    // Trimming would otherwise quietly discard a leading newline, and name trimming
    // would swallow a trailing carriage return, so a caller could hand this a whole
    // block of output and have it read as one line. Control scalars are refused here,
    // before any trim or grammar step can hide one.
    // Console decoding preserves invalid bytes as U+FFFD for display. The string
    // API cannot distinguish that marker from a literal U+FFFD, so refuse both:
    // neither can safely establish an intact player-list response.
    if stripped.chars().any(|c| c.is_control() || c == '\u{fffd}') {
        return Err(PlayerListError::Malformed);
    }
    let trimmed = stripped.trim();
    // A console log prefix such as "[12:00:01] [Server thread/INFO]: " precedes the
    // message. Only a suffix that itself begins a player list is accepted, so an
    // unrelated line containing "]: " is not silently reinterpreted.
    let body = after_log_prefix(trimmed);
    let Some(rest) = strip_head(body) else {
        return Err(PlayerListError::NotRecognized);
    };
    let (online, max, names) = read_counts(rest)?;
    let players = read_names(names)?;
    // The two impossible directions are refused: claiming players while listing none,
    // and listing more names than were claimed online. A count that merely exceeds
    // the visible names is allowed, because a server may hide names by policy.
    if players.len() > online as usize || (online > 0 && players.is_empty()) {
        return Err(PlayerListError::Inconsistent);
    }
    Ok(PlayerList {
        online,
        max,
        players,
    })
}

/// ASCII-case-insensitive prefix strip returning the remainder.
///
/// `get` keeps this panic-free on a non-ASCII byte boundary: a prefix length that
/// would split a `char` simply fails to match. The grammar tokens are all ASCII, so
/// no Unicode case folding is involved.
fn strip_prefix_ascii<'a>(text: &'a str, prefix: &str) -> Option<&'a str> {
    let head = text.get(..prefix.len())?;
    head.eq_ignore_ascii_case(prefix)
        .then(|| &text[prefix.len()..])
}

/// After any accepted log prefix, return the message body.
///
/// Only a genuinely bracketed prefix counts: the line must begin with `[`. Otherwise
/// any message containing the bytes `chat]: There are ` would be accepted as a list,
/// which is a coincidence of wording rather than a console log prefix. Within a
/// bracketed line only the first `]: ` can close a prefix, so a later one inside the
/// message body is not re-examined.
fn after_log_prefix(line: &str) -> &str {
    if !line.starts_with('[') {
        return line;
    }
    let Some(index) = line.find("]: ") else {
        return line;
    };
    // `index + 3` is always a char boundary: it follows three ASCII bytes.
    let candidate = line[index + 3..].trim();
    // Modern vanilla routes console command feedback through System chat.
    // Only the anchored server INFO envelope may carry this wrapper.
    let candidate = if crate::readiness::server_info_message(line).is_some() {
        candidate.strip_prefix("System chat: ").unwrap_or(candidate)
    } else {
        candidate
    };
    if strip_prefix_ascii(candidate, "There are").is_some() {
        candidate
    } else {
        line
    }
}

/// Match "There are" plus its required separator, or `None` when this is not a list line.
///
/// The separator is required. Accepting "There are1/20 ..." would let a different
/// message that merely begins with the same letters be read as a player list.
fn strip_head(body: &str) -> Option<&str> {
    let rest = strip_prefix_ascii(body, "There are")?;
    let after = rest.strip_prefix(' ')?;
    Some(after.trim_start())
}

/// Remove Minecraft legacy section codes and ANSI SGR sequences.
///
/// Only recognized sequences are removed. An unknown or truncated escape is
/// `Malformed`, never dropped: silently discarding it would let a partially corrupt
/// line through as if it were whole, and accepting an arbitrary follow-on scalar
/// would delete real text.
fn strip_formatting(text: &str) -> Result<String> {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\u{a7}' => strip_section(&mut chars)?,
            '\u{1b}' => strip_ansi(&mut chars)?,
            _ => out.push(c),
        }
    }
    Ok(out)
}

/// Consume one legacy section escape.
///
/// Two forms are understood. A single code is the legacy colour and style set:
/// `0`-`9`, `A`-`F`, `K`-`O` and `R`, in either case, because a server may emit
/// either and both denote the same style.
///
/// The hex form is `X` (or `x`) followed by exactly six further section escapes, each
/// carrying one hex digit. That is how the format is actually written, for example
/// `§x§f§f§0§0§a§a`; it is not a marker followed by six bare digits. A run that stops
/// short or carries a non-hex digit is refused rather than partly consumed, so text
/// following a real colour code is never silently swallowed to "complete" it.
fn strip_section<I>(chars: &mut Peekable<I>) -> Result<()>
where
    I: Iterator<Item = char>,
{
    let Some(code) = chars.peek().copied() else {
        // A section sign with nothing after it is truncated.
        return Err(PlayerListError::Malformed);
    };
    if matches!(code, 'X' | 'x') {
        chars.next();
        for _ in 0..6 {
            // Each digit arrives as its own section escape.
            if chars.next() != Some('\u{a7}') {
                return Err(PlayerListError::Malformed);
            }
            match chars.next() {
                Some(digit) if digit.is_ascii_hexdigit() => {}
                _ => return Err(PlayerListError::Malformed),
            }
        }
        return Ok(());
    }
    if matches!(code, '0'..='9' | 'a'..='f' | 'k'..='o' | 'r' | 'A'..='F' | 'K'..='O' | 'R') {
        chars.next();
        return Ok(());
    }
    Err(PlayerListError::Malformed)
}

/// Consume one complete ANSI SGR sequence after `ESC`, or refuse it.
///
/// Only `ESC [ <params> m` is understood. Every other escape is refused: consuming a
/// following scalar to "make progress" would silently delete text such as an emoji or
/// a newline, and accepting a non-SGR CSI such as a cursor move would erase content
/// without changing colour.
fn strip_ansi<I>(chars: &mut Peekable<I>) -> Result<()>
where
    I: Iterator<Item = char>,
{
    if chars.next() != Some('[') {
        // A non-CSI escape, or a truncated one, is not understood here.
        return Err(PlayerListError::Malformed);
    }
    while let Some(c) = chars.peek().copied() {
        match c {
            '0'..='9' | ';' | ':' => {
                chars.next();
            }
            'm' => {
                chars.next();
                return Ok(());
            }
            _ => return Err(PlayerListError::Malformed),
        }
    }
    Err(PlayerListError::Malformed)
}

/// Read `<online>` and `<max>` in either supported form, returning the names tail.
///
/// Two shapes are accepted: `<online>/<max>`, where spaces are legal on either side
/// of the slash, and `<online> of a max of <max>`. Every separator is its own required
/// token, so an absent one is refused rather than absorbed by trimming. The separator
/// is the ASCII space only; the Java `\s+` forms also admitted tabs and newlines, and
/// allowing that here would let a multi-line block parse as a single line.
fn read_counts(rest: &str) -> Result<(u32, u32, &str)> {
    let mut cursor = rest;
    let online = read_number(&mut cursor)?;

    // A slash may follow the count immediately; otherwise a space must separate it
    // from either the slash or the words "of a max of".
    let spaced = if cursor.starts_with('/') {
        cursor
    } else {
        strip_space(cursor)?
    };
    let (max, tail) = match spaced.strip_prefix('/') {
        Some(slash_tail) => {
            let mut rest = slash_tail.trim_start();
            let max = read_number(&mut rest)?;
            (max, rest)
        }
        None => {
            let words =
                strip_prefix_ascii(spaced, "of a max of").ok_or(PlayerListError::Malformed)?;
            // "of a max of" is followed by its own required separator.
            let mut rest = strip_space(words)?;
            let max = read_number(&mut rest)?;
            (max, rest)
        }
    };

    let tail = strip_space(tail)?;
    let body = strip_prefix_ascii(tail, "players online:").ok_or(PlayerListError::Malformed)?;
    // An empty tail is the legal "nobody online" form, but a nonempty one must be
    // separated from the colon, so "players online:Steve" is not this grammar.
    let names = match body.strip_prefix(' ') {
        Some(names) => names.trim(),
        None if body.is_empty() => "",
        None => return Err(PlayerListError::Malformed),
    };
    Ok((online, max, names))
}

/// Consume exactly the whitespace separating two grammar tokens.
///
/// At least one space is required; any further whitespace is tolerated.
fn strip_space(rest: &str) -> Result<&str> {
    rest.strip_prefix(' ')
        .ok_or(PlayerListError::Malformed)
        .map(str::trim_start)
}

/// Read one decimal number, leaving the text from the first non-digit onwards.
///
/// This never signs a value and never returns a fabricated zero: an empty, non-numeric
/// or absurdly long field is refused. Trailing whitespace is left for the caller's
/// separator to consume.
fn read_number(rest: &mut &str) -> Result<u32> {
    let end = rest
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(rest.len());
    let digits = &rest[..end];
    if digits.is_empty() {
        return Err(PlayerListError::Malformed);
    }
    // A count longer than this cannot be a real capacity, and bounds the parse itself.
    if digits.len() > 10 {
        return Err(PlayerListError::NumberOverflow);
    }
    let value: u32 = digits
        .parse()
        .map_err(|_| PlayerListError::NumberOverflow)?;
    *rest = &rest[end..];
    Ok(value)
}

/// Split the names tail on commas into checked entries.
///
/// A wholly empty tail is the one legal absence of tokens and yields no entries. Once
/// a token exists, every token must exist too: an empty interior or trailing token is
/// refused, because "Steve,,Alex" and "Steve," are not a two-name and a one-name list,
/// they are a list whose shape cannot be trusted.
fn read_names(names: &str) -> Result<Vec<PlayerEntry>> {
    if names.is_empty() {
        return Ok(Vec::new());
    }
    let mut entries: Vec<PlayerEntry> = Vec::new();
    let mut tokens = 0usize;
    for token in names.split(',') {
        let raw = token.trim();
        if raw.is_empty() {
            return Err(PlayerListError::Malformed);
        }
        tokens += 1;
        if tokens > MAX_PLAYERS {
            return Err(PlayerListError::TooManyPlayers);
        }
        if let Some((name, uuid)) = split_name_and_uuid(raw) {
            check_name(&name)?;
            entries.push(PlayerEntry {
                name,
                uuid: Some(uuid),
            });
        } else if is_uuid(raw) {
            // A bare UUID carries no visible name. It is dropped rather than given a
            // fabricated one, and it still consumed one of the bounded token slots.
            continue;
        } else {
            check_name(raw)?;
            entries.push(PlayerEntry {
                name: raw.to_owned(),
                uuid: None,
            });
        }
    }
    Ok(entries)
}

/// Split a trailing "(uuid)" from a name, returning a lowercase canonical UUID.
fn split_name_and_uuid(raw: &str) -> Option<(String, String)> {
    let open = raw.rfind('(')?;
    if !raw.ends_with(')') {
        return None;
    }
    let name = raw[..open].trim();
    let uuid = &raw[open + 1..raw.len() - 1];
    if name.is_empty() || !is_uuid(uuid) {
        return None;
    }
    Some((name.to_owned(), uuid.to_ascii_lowercase()))
}

fn is_uuid(text: &str) -> bool {
    const GROUPS: [usize; 5] = [8, 4, 4, 4, 12];
    let mut parts = text.split('-');
    for width in GROUPS {
        let Some(part) = parts.next() else {
            return false;
        };
        if part.len() != width || !part.bytes().all(|b| b.is_ascii_hexdigit()) {
            return false;
        }
    }
    parts.next().is_none()
}

/// Conservative name acceptance: bounded, free of control characters and format marks.
fn check_name(name: &str) -> Result<()> {
    if name.len() > MAX_NAME_BYTES {
        return Err(PlayerListError::Malformed);
    }
    if name
        .chars()
        .any(|c| c.is_control() || c == '\u{a7}' || c == '\u{1b}')
    {
        return Err(PlayerListError::Malformed);
    }
    // A second response can arrive before the first writer's newline. Do not
    // reinterpret its header or log/body separator as part of one composite name.
    if ["There are ", "players online:", "]: "]
        .iter()
        .any(|marker| {
            name.as_bytes()
                .windows(marker.len())
                .any(|part| part.eq_ignore_ascii_case(marker.as_bytes()))
        })
    {
        return Err(PlayerListError::Malformed);
    }
    Ok(())
}

#[cfg(test)]
#[path = "players_tests.rs"]
mod tests;
