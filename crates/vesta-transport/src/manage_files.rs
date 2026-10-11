//! Bounded game-root files. Authentication is never inferred from a path or hash.
use super::{Request, decode, envelope, error, management_error, ok};
use crate::{Reply, response};
use http::{Method, StatusCode};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, time::UNIX_EPOCH};
use vesta_control::{
    files::{
        EntryKind, FileError, MAX_READ_BYTES, MAX_UPLOAD_CHUNK_BYTES, RelativePath,
        RemoteFileMutation, UploadId, WritePrecondition,
    },
    host::NetworkHost,
    identity::Capability,
};

pub(super) fn implemented(path: &str) -> bool {
    matches!(
        path,
        "/api/v3/manage/files"
            | "/api/v3/manage/files/content"
            | "/api/v3/manage/files/tail"
            | "/api/v3/manage/files/lines"
            | "/api/v3/manage/files/follow"
            | "/api/v3/manage/files/mkdir"
            | "/api/v3/manage/files/rename"
            | "/api/v3/manage/files/uploads"
            | "/api/v3/manage/trash"
            | "/api/v3/manage/content/install"
            | "/api/v3/manage/config/server-properties"
    ) || upload_route(path).is_some()
        || restore_route(path).is_some()
}
fn upload_route(path: &str) -> Option<(&str, bool)> {
    let tail = path.strip_prefix("/api/v3/manage/files/uploads/")?;
    let (id, finish) = tail
        .strip_suffix("/finish")
        .map_or((tail, false), |id| (id, true));
    (!id.is_empty() && !id.contains('/')).then_some((id, finish))
}
fn restore_route(path: &str) -> Option<&str> {
    let id = path
        .strip_prefix("/api/v3/manage/trash/")?
        .strip_suffix("/restore")?;
    (!id.is_empty() && !id.contains('/')).then_some(id)
}
pub(crate) fn file_error(value: FileError) -> Reply {
    match value {
        FileError::Control(_) => error(StatusCode::FORBIDDEN, "management_forbidden"),
        FileError::InvalidPath
        | FileError::ProtectedPath
        | FileError::LinkedPath
        | FileError::InvalidLimit
        | FileError::NotRegular
        | FileError::NotDirectory => error(StatusCode::BAD_REQUEST, "invalid_request"),
        FileError::NotFound => error(StatusCode::NOT_FOUND, "resource_not_found"),
        FileError::Conflict(current) => envelope(
            StatusCode::PRECONDITION_FAILED,
            json!({"error":{"code":"precondition_failed","current_sha256":current}}),
        ),
        FileError::ConfirmationRequired => error(StatusCode::BAD_REQUEST, "confirmation_required"),
        FileError::ConfirmationInvalid => error(StatusCode::CONFLICT, "confirmation_invalid"),
        FileError::IdempotencyConflict => error(StatusCode::CONFLICT, "idempotency_conflict"),
        FileError::RecoveryRequired(_) => error(StatusCode::CONFLICT, "recovery_required"),
        FileError::TooLarge => error(StatusCode::PAYLOAD_TOO_LARGE, "request_too_large"),
        _ => error(StatusCode::SERVICE_UNAVAILABLE, "temporarily_unavailable"),
    }
}
fn hash(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || matches!(b, b'a'..=b'f'))
}
fn precondition(request: &Request<'_>) -> Option<WritePrecondition> {
    match (request.if_match, request.if_none_match) {
        (Some(value), None) if hash(value) => Some(WritePrecondition::Matches(value.to_owned())),
        (None, Some("*")) => Some(WritePrecondition::Absent),
        _ => None,
    }
}
fn scoped_key(
    request: &Request<'_>,
    host: &NetworkHost,
    subject: &vesta_control::identity::Subject,
) -> Option<String> {
    let key = request.idempotency?;
    if !(16..=128).contains(&key.len()) || !key.bytes().all(|b| b.is_ascii_graphic()) {
        return None;
    }
    let mut hash = Sha256::new();
    for part in [
        host.management().server_id(),
        subject.principal.as_str(),
        subject.device.as_str(),
        request.method.as_str(),
        request.path,
        key,
    ] {
        hash.update((part.len() as u64).to_be_bytes());
        hash.update(part.as_bytes());
    }
    Some(format!("http:{:x}", hash.finalize()))
}
fn unescape(value: &str) -> Option<String> {
    let mut bytes = Vec::with_capacity(value.len());
    let mut input = value.bytes();
    while let Some(byte) = input.next() {
        match byte {
            b'%' => {
                let a = (input.next()? as char).to_digit(16)?;
                let b = (input.next()? as char).to_digit(16)?;
                bytes.push((a * 16 + b) as u8);
            }
            b'+' => bytes.push(b' '),
            _ => bytes.push(byte),
        }
    }
    String::from_utf8(bytes).ok()
}
fn query(value: Option<&str>, allowed: &[&str]) -> Option<BTreeMap<String, String>> {
    let mut result = BTreeMap::new();
    if let Some(value) = value {
        if value.is_empty() || value.len() > 8192 {
            return None;
        }
        for pair in value.split('&') {
            let (key, value) = pair.split_once('=')?;
            if !allowed.contains(&key) || result.insert(key.to_owned(), unescape(value)?).is_some()
            {
                return None;
            }
        }
    }
    Some(result)
}
fn decimal(value: &str) -> Option<u64> {
    if value.is_empty()
        || value.len() > 20
        || !value.bytes().all(|b| b.is_ascii_digit())
        || (value.len() > 1 && value.starts_with('0'))
    {
        return None;
    }
    value.parse().ok()
}
fn relative(value: &str) -> Option<RelativePath> {
    let path = RelativePath::parse(value).ok()?;
    // Admission documents have their own structured/configuration surface. Raw
    // editing would bypass redaction, schema checks and weakening confirmation.
    if path.as_str().split('/').any(|p| {
        [
            "server.properties",
            "whitelist.json",
            "banned-players.json",
            "banned-ips.json",
            "ops.json",
        ]
        .iter()
        .any(|name| p.eq_ignore_ascii_case(name))
    }) {
        return None;
    }
    Some(path)
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Mkdir {
    path: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Rename {
    from: String,
    to: String,
    expected_sha256: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Begin {
    path: String,
    total_size: String,
    sha256: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Empty {}

pub(super) fn dispatch(host: &NetworkHost, request: Request<'_>) -> Reply {
    let Some(token) = request.authorization else {
        return error(StatusCode::UNAUTHORIZED, "management_session_invalid");
    };
    let subject = match host.management().authenticate(token) {
        Ok(s) => s,
        Err(e) => return management_error(e),
    };
    let action = request
        .path
        .strip_prefix("/api/v3/manage/")
        .expect("file route");
    if action == "content/install" || action == "config/server-properties" {
        return documents(host, &request, &subject, token, action);
    }
    let capabilities: &[Capability] = match action {
        "files" if request.method == Method::GET => &[Capability::FilesList],
        "files" => &[Capability::FilesDelete],
        "files/content" | "files/tail" | "files/lines" | "files/follow"
            if request.method == Method::GET =>
        {
            &[Capability::FilesRead]
        }
        "files/content" | "files/uploads" if request.if_match.is_some() => {
            &[Capability::FilesOverwrite]
        }
        "files/content" | "files/uploads" => &[Capability::FilesCreate],
        "files/mkdir" => &[Capability::FilesMkdir],
        "files/rename" => &[Capability::FilesRename],
        "trash" => &[Capability::FilesRead],
        _ if restore_route(request.path).is_some() => &[Capability::FilesCreate],
        _ => &[Capability::FilesCreate, Capability::FilesOverwrite],
    };
    if let Err(e) = host.files().check_remote_surface(&subject, capabilities) {
        return file_error(e);
    }
    if (request.range.is_some() || request.if_range.is_some())
        && !(action == "files/content" && request.method == Method::GET)
    {
        return error(StatusCode::BAD_REQUEST, "invalid_request");
    }
    if request.method == Method::POST && request.bytes.len() > 65_536 {
        return error(StatusCode::PAYLOAD_TOO_LARGE, "request_too_large");
    }
    if request.method == Method::GET {
        if request.if_match.is_some()
            || request.if_none_match.is_some()
            || !request.bytes.is_empty()
        {
            return error(StatusCode::BAD_REQUEST, "invalid_request");
        }
        if matches!(action, "files/tail" | "files/lines") {
            return text_page(host, &request, &subject, action);
        }
        if action == "files/content" {
            let Some(query) = query(request.query, &["path"]) else {
                return error(StatusCode::BAD_REQUEST, "invalid_request");
            };
            let Some(path) = query.get("path").and_then(|s| relative(s)) else {
                return error(StatusCode::BAD_REQUEST, "invalid_request");
            };
            if let Some(range) = request.range {
                return ranged(host, &subject, &path, range, request.if_range);
            }
            if request.if_range.is_some() {
                return error(StatusCode::BAD_REQUEST, "invalid_request");
            }
            return match host.files().read(&subject, &path, MAX_READ_BYTES) {
                Ok(value) => {
                    // ReplyBody already sends owned bytes in bounded 64-KiB frames.
                    let mut reply = response(
                        StatusCode::OK,
                        value.bytes().to_vec(),
                        "application/octet-stream",
                    );
                    reply.headers_mut().insert(
                        "x-vesta-encoding",
                        http::HeaderValue::from_static(vesta_control::files::encoding_hint(
                            value.bytes(),
                        )),
                    );
                    reply.headers_mut().insert(
                        "x-vesta-file-size",
                        value.bytes().len().to_string().parse().expect("size"),
                    );
                    reply
                        .headers_mut()
                        .insert("x-vesta-sha256", value.sha256().parse().expect("digest"));
                    reply.headers_mut().insert(
                        "etag",
                        format!("\"{}\"", value.sha256()).parse().expect("digest"),
                    );
                    reply
                        .headers_mut()
                        .insert("x-vesta-api-version", http::HeaderValue::from_static("3"));
                    reply.headers_mut().insert(
                        "x-vesta-request-id",
                        format!(
                            "request-{}",
                            super::REQUEST.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
                        )
                        .parse()
                        .expect("request id"),
                    );
                    reply
                }
                Err(e) => file_error(e),
            };
        }
        if action == "files" || action == "trash" {
            let allowed: &[&str] = if action == "files" {
                &["path", "cursor", "limit"]
            } else {
                &["cursor", "limit"]
            };
            let Some(query) = query(request.query, allowed) else {
                return error(StatusCode::BAD_REQUEST, "invalid_request");
            };
            let limit = match query.get("limit") {
                Some(s) => decimal(s).and_then(|n| usize::try_from(n).ok()),
                None => Some(50),
            };
            let Some(limit @ 1..=100) = limit else {
                return error(StatusCode::BAD_REQUEST, "invalid_request");
            };
            let directory = match query.get("path") {
                Some(s) if !s.is_empty() => match relative(s) {
                    Some(p) => Some(p),
                    None => return error(StatusCode::BAD_REQUEST, "invalid_request"),
                },
                _ => None,
            };
            let items: Vec<Value> = if action == "files" {
                match host.files().list(&subject,directory.as_ref()) {
                    Ok(page) => page.entries.into_iter().map(|e| json!({"name":e.name,"kind":match e.kind { EntryKind::File => "file",EntryKind::Directory => "directory" },"size":e.size.to_string(),"modified_at":e.modified_at.and_then(|t| t.duration_since(UNIX_EPOCH).ok()).and_then(|d| i64::try_from(d.as_millis()).ok())})).collect(),
                    Err(e) => return file_error(e),
                }
            } else {
                match host.files().trash_entries(&subject) {
                    Ok(entries) => entries.into_iter().map(|e| json!({"id":e.id,"path":e.path.as_str(),"sha256":e.sha256,"size":e.size.to_string(),"trashed_at_ms":e.trashed_at,"expires_at_ms":e.expires_at})).collect(),
                    Err(e) => return file_error(e),
                }
            };
            let scope = format!(
                "{:x}",
                Sha256::digest(
                    format!(
                        "{}:{action}:{}:{}",
                        host.management().server_id(),
                        subject.device.as_str(),
                        directory.as_ref().map_or("", RelativePath::as_str)
                    )
                    .as_bytes()
                )
            );
            let digest = format!(
                "{:x}",
                Sha256::digest(serde_json::to_vec(&items).expect("JSON"))
            );
            let prefix = format!("{scope}.{digest}.");
            let after = match query.get("cursor") {
                Some(value) if value.len() <= 512 => match value
                    .strip_prefix(&prefix)
                    .and_then(decimal)
                    .and_then(|n| usize::try_from(n).ok())
                {
                    Some(n) => n,
                    None => return error(StatusCode::CONFLICT, "cursor_gap"),
                },
                Some(_) => return error(StatusCode::BAD_REQUEST, "invalid_request"),
                None => 0,
            };
            if after > items.len() {
                return error(StatusCode::CONFLICT, "cursor_gap");
            }
            let end = after.saturating_add(limit).min(items.len());
            return ok(
                json!({"items":items[after..end],"next_cursor":(end<items.len()).then(|| format!("{prefix}{end}"))}),
            );
        }
        return error(StatusCode::BAD_REQUEST, "invalid_request");
    }
    let Some(key) = scoped_key(&request, host, &subject) else {
        return error(StatusCode::BAD_REQUEST, "invalid_request");
    };
    let invalid = || error(StatusCode::BAD_REQUEST, "invalid_request");
    let result = match (request.method.as_str(), action) {
        ("PUT", "files/content") => {
            let Some(query) = query(request.query, &["path"]) else {
                return invalid();
            };
            let Some(path) = query.get("path").and_then(|s| relative(s)) else {
                return invalid();
            };
            let Some(precondition) = precondition(&request) else {
                return invalid();
            };
            host.files().mutate_remote(
                &subject,
                &key,
                RemoteFileMutation::Write {
                    path: &path,
                    precondition: &precondition,
                    bytes: request.bytes,
                },
            )
        }
        ("DELETE", "files") => {
            let Some(query) = query(request.query, &["path"]) else {
                return invalid();
            };
            let Some(path) = query.get("path").and_then(|s| relative(s)) else {
                return invalid();
            };
            let Some(WritePrecondition::Matches(expected)) = precondition(&request) else {
                return invalid();
            };
            if !request.bytes.is_empty() {
                return invalid();
            }
            host.files().mutate_remote(
                &subject,
                &key,
                RemoteFileMutation::Delete {
                    path: &path,
                    expected_sha256: &expected,
                },
            )
        }
        ("POST", "files/mkdir") => {
            if request.query.is_some()
                || request.if_match.is_some()
                || request.if_none_match.is_some()
            {
                return invalid();
            }
            let Some(input) = decode::<Mkdir>(request.bytes) else {
                return invalid();
            };
            let Some(path) = relative(&input.path) else {
                return invalid();
            };
            host.files()
                .mutate_remote(&subject, &key, RemoteFileMutation::Mkdir { path: &path })
        }
        ("POST", "files/rename") => {
            if request.query.is_some()
                || request.if_match.is_some()
                || request.if_none_match.is_some()
            {
                return invalid();
            }
            let Some(input) = decode::<Rename>(request.bytes) else {
                return invalid();
            };
            let (Some(from), Some(to)) = (relative(&input.from), relative(&input.to)) else {
                return invalid();
            };
            if !hash(&input.expected_sha256) {
                return invalid();
            }
            host.files().mutate_remote(
                &subject,
                &key,
                RemoteFileMutation::Rename {
                    from: &from,
                    to: &to,
                    expected_sha256: &input.expected_sha256,
                },
            )
        }
        ("POST", "files/uploads") => {
            if request.query.is_some() {
                return invalid();
            }
            let Some(input) = decode::<Begin>(request.bytes) else {
                return invalid();
            };
            let Some(path) = relative(&input.path) else {
                return invalid();
            };
            let Some(total_size) = decimal(&input.total_size) else {
                return invalid();
            };
            let Some(precondition) = precondition(&request) else {
                return invalid();
            };
            if !hash(&input.sha256) {
                return invalid();
            }
            host.files().mutate_remote(
                &subject,
                &key,
                RemoteFileMutation::Begin {
                    path: &path,
                    precondition: &precondition,
                    total_size,
                    sha256: &input.sha256,
                },
            )
        }
        _ if upload_route(request.path).is_some() => {
            let (id, finish) = upload_route(request.path).expect("upload route");
            let Ok(id) = UploadId::parse(id) else {
                return invalid();
            };
            if request.if_match.is_some() || request.if_none_match.is_some() {
                return invalid();
            }
            let mutation = if finish && request.method == Method::POST {
                if request.query.is_some() || decode::<Empty>(request.bytes).is_none() {
                    return invalid();
                }
                RemoteFileMutation::Finish { id: &id }
            } else if !finish && request.method == Method::PUT {
                let Some(query) = query(request.query, &["offset"]) else {
                    return invalid();
                };
                let Some(offset) = query.get("offset").and_then(|s| decimal(s)) else {
                    return invalid();
                };
                if request.bytes.len() > MAX_UPLOAD_CHUNK_BYTES {
                    return error(StatusCode::PAYLOAD_TOO_LARGE, "request_too_large");
                }
                RemoteFileMutation::Append {
                    id: &id,
                    offset,
                    bytes: request.bytes,
                }
            } else if !finish && request.method == Method::DELETE {
                if request.query.is_some() || !request.bytes.is_empty() {
                    return invalid();
                }
                RemoteFileMutation::Abort { id: &id }
            } else {
                return invalid();
            };
            host.files().mutate_remote(&subject, &key, mutation)
        }
        _ if restore_route(request.path).is_some() && request.method == Method::POST => {
            let id = restore_route(request.path).expect("restore route");
            if request.query.is_some()
                || request.if_match.is_some()
                || request.if_none_match.is_some()
                || decode::<Empty>(request.bytes).is_none()
            {
                return invalid();
            }
            host.files()
                .mutate_remote(&subject, &key, RemoteFileMutation::Restore { id })
        }
        _ => return invalid(),
    };
    match result {
        Ok(value) => ok(value),
        Err(e) => file_error(e),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Install {
    provider: String,
    project_id: String,
    version_id: String,
}
fn documents(
    host: &NetworkHost,
    request: &Request<'_>,
    subject: &vesta_control::identity::Subject,
    token: &str,
    action: &str,
) -> Reply {
    let capability = if action == "content/install" {
        Capability::ContentInstall
    } else if request.method == Method::GET {
        Capability::ConfigView
    } else {
        Capability::ConfigEdit
    };
    if let Err(e) = host.management().authorize(token, capability, None) {
        return management_error(e);
    }
    let caps: &[Capability] = if action == "content/install" {
        &[Capability::FilesCreate]
    } else if request.method == Method::GET {
        &[Capability::FilesRead]
    } else {
        &[Capability::FilesOverwrite]
    };
    if let Err(e) = host.files().check_remote_surface(subject, caps) {
        return file_error(e);
    }
    if request.query.is_some() || request.if_match.is_some() || request.if_none_match.is_some() {
        return error(StatusCode::BAD_REQUEST, "invalid_request");
    }
    if request.bytes.len() > 65_536 {
        return error(StatusCode::PAYLOAD_TOO_LARGE, "request_too_large");
    }
    if action == "config/server-properties" && request.method == Method::GET {
        if !request.bytes.is_empty() {
            return error(StatusCode::BAD_REQUEST, "invalid_request");
        }
        return match vesta_control::config::ServerPropertiesEditor::new(host.files()).read(subject)
        {
            Ok(value) => ok(value),
            Err(e) => file_error(e),
        };
    }
    let Some(key) = scoped_key(request, host, subject) else {
        return error(StatusCode::BAD_REQUEST, "invalid_request");
    };
    if action == "config/server-properties" && request.method == Method::PATCH {
        if let Err(e) = host
            .files()
            .check_remote_surface(subject, &[Capability::FilesRead])
        {
            return file_error(e);
        }
        let Some(input) = decode::<vesta_control::config::PropertiesPatch>(request.bytes) else {
            return error(StatusCode::BAD_REQUEST, "invalid_request");
        };
        return match vesta_control::config::ServerPropertiesEditor::new(host.files())
            .patch(subject, &key, input)
        {
            Ok(value) if value["state"] == "awaiting_confirmation" => {
                envelope(StatusCode::ACCEPTED, json!({"data":value}))
            }
            Ok(value) => ok(value),
            Err(e) => file_error(e),
        };
    }
    if action == "content/install" && request.method == Method::POST {
        use vesta_control::content::{
            ContentInstaller, InstallRequest, Provider, TrustedSourceRegistry, UreqDownloadHttp,
            UreqProviderClient,
        };
        let Some(input) = decode::<Install>(request.bytes) else {
            return error(StatusCode::BAD_REQUEST, "invalid_request");
        };
        let provider = match input.provider.as_str() {
            "modrinth" => Provider::Modrinth,
            "curseforge" => Provider::Curseforge,
            _ => return error(StatusCode::BAD_REQUEST, "invalid_request"),
        };
        let registry = TrustedSourceRegistry::default();
        // This optional credential is trusted operator environment, never a request
        // field; the provider adapter sends it only to its fixed metadata host.
        let credential = std::env::var("VESTA_CURSEFORGE_API_KEY")
            .ok()
            .filter(|key| {
                !key.is_empty() && key.len() <= 1024 && key.bytes().all(|b| b.is_ascii_graphic())
            });
        let providers = UreqProviderClient::new(credential);
        let http = UreqDownloadHttp::default();
        let installer = ContentInstaller::new(host.files(), &providers, &http, &registry);
        let input = InstallRequest::Provider {
            provider,
            project_id: input.project_id,
            version_id: input.version_id,
        };
        return match installer.install_remote(subject, &input, &key) {
            Ok(value) => ok(value),
            Err(vesta_control::content::ContentError::InvalidRequest) => {
                error(StatusCode::BAD_REQUEST, "invalid_request")
            }
            Err(vesta_control::content::ContentError::File(e)) => file_error(e),
            Err(_) => error(StatusCode::SERVICE_UNAVAILABLE, "temporarily_unavailable"),
        };
    }
    error(StatusCode::BAD_REQUEST, "invalid_request")
}

fn parse_range(value: &str, size: u64) -> Option<(u64, u64)> {
    let value = value.strip_prefix("bytes=")?;
    let (start, end) = value.split_once('-')?;
    if size == 0 {
        return None;
    }
    let (start, end) = if start.is_empty() {
        let suffix = decimal(end)?;
        if suffix == 0 {
            return None;
        }
        (size.saturating_sub(suffix), size - 1)
    } else {
        let start = decimal(start)?;
        let end = if end.is_empty() {
            size - 1
        } else {
            decimal(end)?.min(size - 1)
        };
        (start, end)
    };
    if start >= size || end < start || end - start + 1 > vesta_control::files::MAX_RANGE_BYTES {
        return None;
    }
    Some((start, end))
}
fn ranged(
    host: &NetworkHost,
    subject: &vesta_control::identity::Subject,
    path: &RelativePath,
    range: &str,
    if_range: Option<&str>,
) -> Reply {
    let mut file = match host.files().open_stream(subject, path) {
        Ok(file) => file,
        Err(e) => return file_error(e),
    };
    let size = file.size();
    let etag = file.etag().to_owned();
    if if_range.is_some_and(|value| value != etag) {
        // If-Range mismatch ignores Range. Preserve the original full-read bound.
        let mut reply = match host.files().read(subject, path, MAX_READ_BYTES) {
            Ok(read) => {
                let current = match host.files().open_stream(subject, path) {
                    Ok(current) => current,
                    Err(e) => return file_error(e),
                };
                if current.etag() != etag || read.bytes().len() as u64 != current.size() {
                    let mut reply = error(StatusCode::PRECONDITION_FAILED, "precondition_failed");
                    read_headers(&mut reply, current.size(), current.etag());
                    return reply;
                }
                let mut reply = response(
                    StatusCode::OK,
                    read.bytes().to_vec(),
                    "application/octet-stream",
                );
                reply
                    .headers_mut()
                    .insert("x-vesta-sha256", read.sha256().parse().expect("digest"));
                reply.headers_mut().insert(
                    "x-vesta-encoding",
                    http::HeaderValue::from_static(vesta_control::files::encoding_hint(
                        read.bytes(),
                    )),
                );
                reply
            }
            Err(e) => file_error(e),
        };
        read_headers(&mut reply, size, &etag);
        return reply;
    }
    let Some((start, end)) = parse_range(range, size) else {
        let mut reply = error(StatusCode::RANGE_NOT_SATISFIABLE, "range_not_satisfiable");
        reply.headers_mut().insert(
            "content-range",
            format!("bytes */{size}").parse().expect("size"),
        );
        read_headers(&mut reply, size, &etag);
        return reply;
    };
    let hint = match file.encoding_hint() {
        Ok(hint) => hint,
        Err(e) => return file_error(e),
    };
    let Some(body) = crate::file_body::FileBody::new(file, start, end - start + 1) else {
        return error(StatusCode::SERVICE_UNAVAILABLE, "temporarily_unavailable");
    };
    let mut reply = http::Response::builder()
        .status(StatusCode::PARTIAL_CONTENT)
        .header("content-type", "application/octet-stream")
        .header("cache-control", "no-store")
        .header("content-length", end - start + 1)
        .header("content-range", format!("bytes {start}-{end}/{size}"))
        .header("x-vesta-encoding", hint)
        .body(crate::ReplyBody::File(Box::new(body)))
        .expect("range headers");
    read_headers(&mut reply, size, &etag);
    reply
}
fn read_headers(reply: &mut Reply, size: u64, etag: &str) {
    for (name, value) in [
        ("etag", etag.to_owned()),
        ("x-vesta-file-size", size.to_string()),
        ("accept-ranges", "bytes".to_owned()),
        ("x-vesta-api-version", "3".to_owned()),
        (
            "x-vesta-request-id",
            format!(
                "request-{}",
                super::REQUEST.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            ),
        ),
    ] {
        reply
            .headers_mut()
            .insert(name, value.parse().expect("fixed read header"));
    }
}
fn text_page(
    host: &NetworkHost,
    request: &Request<'_>,
    subject: &vesta_control::identity::Subject,
    action: &str,
) -> Reply {
    use base64::Engine;
    if request.range.is_some() || request.if_range.is_some() {
        return error(StatusCode::BAD_REQUEST, "invalid_request");
    }
    let tail = action == "files/tail";
    let allowed: &[&str] = if tail {
        &["path", "lines"]
    } else {
        &["path", "from_byte", "limit", "direction"]
    };
    let Some(query) = query(request.query, allowed) else {
        return error(StatusCode::BAD_REQUEST, "invalid_request");
    };
    let Some(path) = query.get("path").and_then(|s| relative(s)) else {
        return error(StatusCode::BAD_REQUEST, "invalid_request");
    };
    let limit = query
        .get(if tail { "lines" } else { "limit" })
        .map_or(Some(100), |s| {
            decimal(s).and_then(|n| usize::try_from(n).ok())
        });
    let Some(limit @ 1..=vesta_control::files::MAX_TEXT_LINES) = limit else {
        return error(StatusCode::BAD_REQUEST, "invalid_request");
    };
    let backwards = if tail {
        true
    } else {
        match query.get("direction").map(String::as_str) {
            Some("backward") => true,
            Some("forward") => false,
            None => !query.contains_key("from_byte"),
            _ => return error(StatusCode::BAD_REQUEST, "invalid_request"),
        }
    };
    let mut file = match host.files().open_stream(subject, &path) {
        Ok(file) => file,
        Err(e) => return file_error(e),
    };
    let from = query
        .get("from_byte")
        .map_or(Some(if backwards { file.size() } else { 0 }), |s| {
            decimal(s)
        });
    let Some(from) = from else {
        return error(StatusCode::BAD_REQUEST, "invalid_request");
    };
    match file.lines(from, limit, backwards) {
        Ok(page) => ok(
            json!({"bytes_base64":base64::engine::general_purpose::STANDARD.encode(&page.bytes),"encoding":page.encoding,"start_byte":page.start.to_string(),"cursor":page.end.to_string(),"next_byte":(page.end < page.size).then(|| page.end.to_string()),"prev_byte":(page.start > 0).then(|| page.start.to_string()),"size":page.size.to_string(),"etag":page.etag}),
        ),
        Err(e) => file_error(e),
    }
}

pub(crate) fn follow_query(value: Option<&str>) -> Option<(RelativePath, Option<u64>)> {
    let query = query(value, &["path", "cursor"])?;
    // Document exclusion occurs after capability admission in open_stream.
    let path = RelativePath::parse(query.get("path")?).ok()?;
    let cursor = query.get("cursor").map(|s| decimal(s)).transpose_option()?;
    Some((path, cursor))
}
trait TransposeOption<T> {
    fn transpose_option(self) -> Option<Option<T>>;
}
impl<T> TransposeOption<T> for Option<Option<T>> {
    fn transpose_option(self) -> Option<Option<T>> {
        match self {
            Some(Some(value)) => Some(Some(value)),
            Some(None) => None,
            None => Some(None),
        }
    }
}
