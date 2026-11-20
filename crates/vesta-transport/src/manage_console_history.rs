use super::*;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::Serialize;
use sha2::{Digest, Sha256};
use vesta_control::files::{FileError, FileStream};
use vesta_control::operations::{
    ConsoleLine, MAX_CONSOLE_LINE_BYTES as MAX_LINE_BYTES, MAX_CONSOLE_PAGE_BYTES as MAX_PAGE_BYTES,
    MAX_CONSOLE_PAGE_LINES as MAX_PAGE_LINES,
};

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum HistoryCursor {
    Log {
        name: String,
        version: String,
        offset: u64,
    },
    Older {
        name: String,
    },
    ArchivedBefore {
        day: String,
    },
}

fn encode(server: &str, value: HistoryCursor) -> String {
    cursor(
        server,
        "console-history",
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(&value).expect("history cursor is serializable")),
    )
}

enum Query {
    Tail,
    Forward(u64),
    Before(u64),
    History(HistoryCursor),
}

fn query(raw: Option<&str>, server: &str) -> Option<(Query, usize)> {
    let Some(raw) = raw else {
        return Some((Query::Tail, 50));
    };
    if raw.len() > 600 || raw.is_empty() {
        return None;
    }
    let mut mode = None;
    let mut limit = None;
    for field in raw.split('&') {
        let (key, value) = field.split_once('=')?;
        match key {
            "limit" if limit.is_none() && value.bytes().all(|b| b.is_ascii_digit()) => {
                limit = Some(value.parse().ok()?);
            }
            "cursor" | "before" if mode.is_none() => {
                if let Some(number) = value.strip_prefix(&format!("{server}.console."))
                    && !number.is_empty()
                    && number.bytes().all(|b| b.is_ascii_digit())
                {
                    let number = number.parse().ok()?;
                    mode = Some(if key == "cursor" {
                        Query::Forward(number)
                    } else {
                        Query::Before(number)
                    });
                } else if key == "before" {
                    let token = value.strip_prefix(&format!("{server}.console-history."))?;
                    mode = Some(Query::History(
                        serde_json::from_slice(&URL_SAFE_NO_PAD.decode(token).ok()?).ok()?,
                    ));
                } else {
                    return None;
                }
            }
            _ => return None,
        }
    }
    let limit = limit.unwrap_or(50);
    (1..=MAX_PAGE_LINES)
        .contains(&limit)
        .then(|| (mode.unwrap_or(Query::Tail), limit))
}

pub(super) fn dispatch(host: &NetworkHost, raw: Option<&str>) -> Reply {
    let server = host.management().server_id();
    let Some((query, limit)) = query(raw, server) else {
        return error(StatusCode::BAD_REQUEST, "invalid_request");
    };
    let runtime = host.runtime();
    if let Query::History(history) = query {
        let latest = match runtime.console_snapshot(0, 1) {
            Ok(page) => page.latest_cursor,
            Err(_) => return error(StatusCode::SERVICE_UNAVAILABLE, "temporarily_unavailable"),
        };
        return render_history(host, history_page(host, history, limit), latest);
    }
    let page = match query {
        Query::Forward(after) => runtime
            .console_snapshot(after, limit)
            .map(|page| (page, None)),
        Query::Tail => runtime.console_before(None, limit),
        Query::Before(before) => runtime.console_before(Some(before), limit),
        Query::History(_) => unreachable!(),
    };
    let (page, oldest) = match page {
        Ok((page, oldest)) if !page.gap => (page, oldest),
        Ok(_) => return error(StatusCode::CONFLICT, "cursor_gap"),
        Err(_) => return error(StatusCode::BAD_REQUEST, "invalid_request"),
    };
    if page.lines.is_empty() {
        if let Query::Before(_) = query {
            return render_history(
                host,
                handoff(host, oldest.as_ref(), limit),
                page.latest_cursor,
            );
        }
        if matches!(query, Query::Tail) {
            let previous = initial_disk_cursor(host, None).map(|disk| encode(server, disk));
            return response(
                host,
                Vec::new(),
                page.next_cursor,
                page.latest_cursor,
                previous,
            );
        }
    }
    let previous = if let Some(first) = page.lines.first() {
        if matches!(query, Query::Forward(_))
            || oldest
                .as_ref()
                .is_some_and(|oldest| first.cursor > oldest.cursor)
        {
            Some(cursor(server, "console", first.cursor))
        } else {
            initial_disk_cursor(host, oldest.as_ref()).map(|disk| encode(server, disk))
        }
    } else {
        None
    };
    let items = page
        .lines
        .into_iter()
        .map(|line| Item {
            cursor: cursor(server, "console", line.cursor),
            text: line.text,
            truncated: line.truncated,
        })
        .collect();
    response(host, items, page.next_cursor, page.latest_cursor, previous)
}

struct Item {
    cursor: String,
    text: String,
    truncated: bool,
}

fn response(
    host: &NetworkHost,
    items: Vec<Item>,
    next: u64,
    latest: u64,
    previous: Option<String>,
) -> Reply {
    let mut redacted = Vec::new();
    for item in items {
        match redact_console(host, &item.text) {
            Ok(text) => {
                redacted.push(json!({"cursor":item.cursor,"text":text,"truncated":item.truncated}))
            }
            Err(error) => return management_error(error),
        }
    }
    let server = host.management().server_id();
    ok(json!({
        "items":redacted, "next_cursor":cursor(server,"console",next),
        "latest_cursor":cursor(server,"console",latest), "previous_cursor":previous,
    }))
}

type DiskPage = (Vec<Item>, Option<String>);

fn render_history(host: &NetworkHost, result: Result<DiskPage, FileError>, latest: u64) -> Reply {
    match result {
        Ok((items, previous)) => response(host, items, latest, latest, previous),
        Err(FileError::Conflict(_) | FileError::NotFound) => {
            error(StatusCode::CONFLICT, "cursor_gap")
        }
        Err(FileError::InvalidPath | FileError::InvalidLimit) => {
            error(StatusCode::BAD_REQUEST, "invalid_request")
        }
        Err(_) => error(StatusCode::SERVICE_UNAVAILABLE, "temporarily_unavailable"),
    }
}

fn history_page(
    host: &NetworkHost,
    at: HistoryCursor,
    limit: usize,
) -> Result<DiskPage, FileError> {
    match at {
        HistoryCursor::Log {
            name,
            version,
            offset,
        } => {
            let mut file = host.files().open_console_history(&name)?;
            if version_for(&name, &mut file)? != version || offset > file.size() {
                return Err(FileError::Conflict(None));
            }
            let names = host.files().console_history_names()?;
            if offset == 0 {
                return from_archives(
                    host,
                    names.iter().filter(|older| older_than(older, &name)),
                    limit,
                );
            }
            disk_page(host, &name, &mut file, version, offset, limit, &names)
        }
        HistoryCursor::Older { name } => {
            if name != "latest.log" && archive_key(&name).is_none() {
                return Err(FileError::InvalidPath);
            }
            let names = host.files().console_history_names()?;
            from_archives(
                host,
                names.iter().filter(|older| older_than(older, &name)),
                limit,
            )
        }
        HistoryCursor::ArchivedBefore { day } => {
            if chrono::NaiveDate::parse_from_str(&day, "%Y-%m-%d").is_err() {
                return Err(FileError::InvalidPath);
            }
            let names = host.files().console_history_names()?;
            from_archives(
                host,
                names
                    .iter()
                    .filter(|name| archive_key(name).is_some_and(|(date, _)| date < day.as_str())),
                limit,
            )
        }
    }
}

fn initial_disk_cursor(host: &NetworkHost, anchor: Option<&ConsoleLine>) -> Option<HistoryCursor> {
    let names = host.files().console_history_names().ok()?;
    if let Ok(mut latest) = host.files().open_console_history("latest.log")
        && latest.size() > 0
    {
        let offset = match anchor {
            Some(anchor) => latest_boundary(&mut latest, anchor).ok().flatten(),
            None => latest_end(&mut latest).ok(),
        };
        if let Some(offset) = offset
            && (offset > 0 || names.len() > 1)
            && let Ok(version) = version_for("latest.log", &mut latest)
        {
            return Some(HistoryCursor::Log {
                name: "latest.log".into(),
                version,
                offset,
            });
        }
    }
    match anchor {
        Some(anchor) => {
            let day = chrono::DateTime::<chrono::Local>::from(anchor.observed_at)
                .date_naive()
                .to_string();
            names
                .iter()
                .any(|name| archive_key(name).is_some_and(|(date, _)| date < day.as_str()))
                .then_some(HistoryCursor::ArchivedBefore { day })
        }
        None => (names.len() > 1).then(|| HistoryCursor::Older {
            name: "latest.log".into(),
        }),
    }
}

fn handoff(
    host: &NetworkHost,
    anchor: Option<&ConsoleLine>,
    limit: usize,
) -> Result<DiskPage, FileError> {
    match initial_disk_cursor(host, anchor) {
        Some(history) => history_page(host, history, limit),
        None => Ok((Vec::new(), None)),
    }
}

fn archive_key(name: &str) -> Option<(&str, u32)> {
    let (date, index) = name.strip_suffix(".log.gz")?.rsplit_once('-')?;
    Some((date, index.parse().ok()?))
}

fn older_than(candidate: &str, name: &str) -> bool {
    archive_key(candidate).is_some_and(|value| {
        name == "latest.log" || archive_key(name).is_some_and(|boundary| value < boundary)
    })
}

fn from_archives<'a>(
    host: &NetworkHost,
    mut names: impl Iterator<Item = &'a String>,
    limit: usize,
) -> Result<DiskPage, FileError> {
    while let Some(name) = names.next() {
        let mut file = match host.files().open_console_history(name) {
            Ok(file) if file.size() > 0 => file,
            Ok(_)
            | Err(
                FileError::NotFound
                | FileError::Corrupt
                | FileError::NotCompressed
                | FileError::TooLarge
                | FileError::LinkedPath
                | FileError::ProtectedPath,
            ) => continue,
            Err(error) => return Err(error),
        };
        let end = file.size();
        let version = version_for(name, &mut file)?;
        let (items, mut previous) = disk_page(host, name, &mut file, version, end, limit, &[])?;
        if previous.is_none() && names.next().is_some() {
            previous = Some(encode(
                host.management().server_id(),
                HistoryCursor::Older { name: name.clone() },
            ));
        }
        return Ok((items, previous));
    }
    Ok((Vec::new(), None))
}

/// Match an entire captured line to a recent log line before returning older
/// bytes. A missing anchor falls back to archives from earlier days.
fn latest_boundary(file: &mut FileStream, anchor: &ConsoleLine) -> Result<Option<u64>, FileError> {
    let mut end = latest_end(file)?;
    for _ in 0..8 {
        if end == 0 {
            break;
        }
        let page = file.lines(end, 1000, true)?;
        let mut offset = page.start;
        let mut found = None;
        for line in page.bytes.split_inclusive(|byte| *byte == b'\n') {
            if log_text(line) == anchor.text {
                found = Some(offset);
            }
            offset += line.len() as u64;
        }
        if found.is_some() {
            return Ok(found);
        }
        end = page.start;
    }
    Ok(None)
}

fn latest_end(file: &mut FileStream) -> Result<u64, FileError> {
    let width = file.size().min(64 * 1024);
    if width == 0 {
        return Ok(0);
    }
    let start = file.size() - width;
    let tail = file.read_chunk(start, width as usize)?;
    Ok(tail
        .iter()
        .rposition(|byte| *byte == b'\n')
        .map_or(0, |at| start + at as u64 + 1))
}

fn version_for(name: &str, file: &mut FileStream) -> Result<String, FileError> {
    if name != "latest.log" {
        return Ok(file.etag().to_owned());
    }
    let prefix = file.read_chunk(0, 128)?;
    let length = prefix
        .iter()
        .position(|byte| *byte == b'\n')
        .map_or(prefix.len(), |at| at + 1);
    let mut hash = Sha256::new();
    hash.update(file.identity());
    hash.update(&prefix[..length]);
    Ok(format!("{:x}", hash.finalize()))
}

fn log_text(bytes: &[u8]) -> std::borrow::Cow<'_, str> {
    let bytes = bytes.strip_suffix(b"\n").unwrap_or(bytes);
    String::from_utf8_lossy(bytes.strip_suffix(b"\r").unwrap_or(bytes))
}

fn disk_page(
    host: &NetworkHost,
    name: &str,
    file: &mut FileStream,
    version: String,
    end: u64,
    limit: usize,
    names: &[String],
) -> Result<DiskPage, FileError> {
    let page = file.lines(end, limit, true)?;
    let mut offset = page.end;
    let mut oldest = offset;
    let mut bytes = 0;
    let mut items = Vec::new();
    let server = host.management().server_id();
    for line in page.bytes.split_inclusive(|byte| *byte == b'\n').rev() {
        offset -= line.len() as u64;
        // A partially written latest line has no stable end until newline.
        if name == "latest.log"
            && offset + line.len() as u64 == file.size()
            && !line.ends_with(b"\n")
        {
            continue;
        }
        let text = log_text(line);
        let length = text.floor_char_boundary(MAX_LINE_BYTES.min(text.len()));
        if bytes + length > MAX_PAGE_BYTES {
            break;
        }
        bytes += length;
        oldest = offset;
        items.push(Item {
            cursor: encode(
                server,
                HistoryCursor::Log {
                    name: name.to_owned(),
                    version: version.clone(),
                    offset,
                },
            ),
            text: text[..length].to_owned(),
            truncated: length < text.len(),
        });
    }
    items.reverse();
    let previous = if oldest > 0 && !items.is_empty() {
        items.first().map(|item| item.cursor.clone())
    } else if names.iter().any(|older| older_than(older, name)) {
        Some(encode(
            server,
            HistoryCursor::Older {
                name: name.to_owned(),
            },
        ))
    } else {
        None
    };
    Ok((items, previous))
}
