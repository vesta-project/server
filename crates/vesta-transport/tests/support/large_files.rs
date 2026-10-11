//! Owned disk/TLS fixtures for large-file viewers and follow sockets.
use super::*;
use base64::Engine;
use tokio_tungstenite::tungstenite::{self, Message, client::IntoClientRequest};
type Peer = tungstenite::WebSocket<TlsStream>;
fn owner_token(f: &Fixture) -> (String, String) {
    let (device, key) = enroll_management(f, vesta_control::identity::BuiltinRole::Owner);
    (management_session(f, &device, &key), device)
}
fn get(f: &Fixture, token: &str, path: &str, headers: &str) -> WireReply {
    f.raw(format!("GET /api/v3/manage/{path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nAuthorization: Bearer {token}\r\n{headers}\r\n").as_bytes())
}
fn follow(f: &Fixture, token: &str, query: &str) -> Result<Peer, u16> {
    let mut request = format!(
        "wss://localhost:{}/api/v3/manage/files/follow?{query}",
        f.address.port()
    )
    .into_client_request()
    .unwrap();
    request
        .headers_mut()
        .insert("authorization", format!("Bearer {token}").parse().unwrap());
    match tungstenite::client(request, f.stream()) {
        Ok((peer, _)) => Ok(peer),
        Err(tungstenite::HandshakeError::Failure(tungstenite::Error::Http(reply))) => {
            Err(reply.status().as_u16())
        }
        Err(error) => panic!("owned follow handshake failed: {error}"),
    }
}
fn notice(peer: &mut Peer) -> serde_json::Value {
    loop {
        match peer.read().unwrap() {
            Message::Text(text) => {
                assert!(text.len() < 24 * 1024);
                return serde_json::from_str(&text).unwrap();
            }
            Message::Ping(_) => peer.flush().unwrap(),
            other => panic!("unexpected follow frame {other:?}"),
        }
    }
}
fn bytes(value: &serde_json::Value) -> Vec<u8> {
    base64::engine::general_purpose::STANDARD
        .decode(value["bytes_base64"].as_str().unwrap())
        .unwrap()
}
#[test]
fn tls_ranges_boundaries_suffix_invalid_and_large_files() {
    let f = Fixture::new("minecraft", 8, 4096, 5);
    let (token, _) = owner_token(&f);
    let content: Vec<_> = (0..5 * 1024 * 1024 + 13).map(|i| (i % 251) as u8).collect();
    std::fs::write(f.temp.path().join("game/latest.log"), &content).unwrap();
    let path = "files/content?path=latest.log";
    management_reply_error(&get(&f, &token, path, ""), 413, "request_too_large");
    for (header, start, end) in [
        ("bytes=0-0".to_owned(), 0, 0),
        ("bytes=65535-65537".into(), 65535, 65537),
        ("bytes=-13".into(), content.len() - 13, content.len() - 1),
        (
            format!("bytes={}-99999999", content.len() - 1),
            content.len() - 1,
            content.len() - 1,
        ),
        ("bytes=0-4194303".into(), 0, 4194303),
    ] {
        let reply = get(&f, &token, path, &format!("Range: {header}\r\n"));
        assert_eq!(reply.status, 206);
        assert_eq!(reply.body, content[start..=end]);
        assert_eq!(
            reply.headers["content-range"],
            format!("bytes {start}-{end}/{}", content.len())
        );
        assert_eq!(
            reply.headers["x-vesta-file-size"],
            content.len().to_string()
        );
        assert_eq!(reply.headers["x-vesta-encoding"], "binary");
        assert_eq!(reply.headers["accept-ranges"], "bytes");
        assert!(!reply.headers["etag"].starts_with("W/"));
    }
    for header in [
        "bytes=-0",
        "bytes=9-8",
        "bytes=99999999-",
        "bytes=0-4194304",
        "bytes=0-1,3-4",
        "items=0-1",
        "bytes=a-b",
        "bytes=18446744073709551616-",
        "bytes=00-1",
    ] {
        let reply = get(&f, &token, path, &format!("Range: {header}\r\n"));
        management_reply_error(&reply, 416, "range_not_satisfiable");
        assert_eq!(
            reply.headers["content-range"],
            format!("bytes */{}", content.len())
        );
    }
    std::fs::write(f.temp.path().join("game/empty.log"), b"").unwrap();
    management_reply_error(
        &get(
            &f,
            &token,
            "files/content?path=empty.log",
            "Range: bytes=0-0\r\n",
        ),
        416,
        "range_not_satisfiable",
    );
    management_reply_error(
        &get(&f, &token, path, "Range: bytes=0-1\r\nRange: bytes=2-3\r\n"),
        400,
        "invalid_request",
    );
}
#[test]
fn tls_if_range_preserves_full_read_fallback_and_detects_change() {
    let f = Fixture::new("minecraft", 8, 4096, 5);
    let (token, _) = owner_token(&f);
    let disk = f.temp.path().join("game/latest.log");
    std::fs::write(&disk, b"first").unwrap();
    let path = "files/content?path=latest.log";
    let first = get(&f, &token, path, "Range: bytes=0-1\r\n");
    let etag = &first.headers["etag"];
    let same = get(
        &f,
        &token,
        path,
        &format!("Range: bytes=0-1\r\nIf-Range: {etag}\r\n"),
    );
    assert_eq!(same.status, 206);
    assert_eq!(same.body, b"fi");
    std::fs::write(&disk, b"changed").unwrap();
    let changed = get(
        &f,
        &token,
        path,
        &format!("Range: bytes=0-1\r\nIf-Range: {etag}\r\n"),
    );
    assert_eq!(changed.status, 200);
    assert_eq!(changed.body, b"changed");
    assert_ne!(changed.headers["etag"], *etag);
    std::fs::write(&disk, vec![b'x'; 2 * 1024 * 1024]).unwrap();
    let changed = get(
        &f,
        &token,
        path,
        &format!("Range: bytes=0-1\r\nIf-Range: {etag}\r\n"),
    );
    management_reply_error(&changed, 413, "request_too_large");
    assert_ne!(changed.headers["etag"], *etag);
    assert_eq!(
        changed.headers["x-vesta-file-size"],
        (2 * 1024 * 1024).to_string()
    );
}
#[test]
fn tls_tail_and_lines_page_complete_unmodified_long_utf8_and_binary_lines() {
    let f = Fixture::new("minecraft", 8, 4096, 5);
    let (token, _) = owner_token(&f);
    let content = [
        b"first\r\n".as_slice(),
        &vec![b'x'; 150_000],
        "\nlast π".as_bytes(),
    ]
    .concat();
    std::fs::write(f.temp.path().join("game/latest.log"), &content).unwrap();
    let tail = get(&f, &token, "files/tail?path=latest.log&lines=2", "");
    assert_eq!(tail.status, 200);
    let tail = tail.json()["data"].clone();
    assert_eq!(bytes(&tail), content[7..]);
    assert_eq!(tail["cursor"], content.len().to_string());
    assert_eq!(tail["prev_byte"], "7");
    assert_eq!(tail["encoding"], "utf-8");
    let previous = get(
        &f,
        &token,
        "files/lines?path=latest.log&from_byte=7&direction=backward&limit=1",
        "",
    );
    assert_eq!(bytes(&previous.json()["data"]), b"first\r\n");
    assert_eq!(previous.json()["data"]["next_byte"], "7");
    let forward = get(
        &f,
        &token,
        "files/lines?path=latest.log&from_byte=7&limit=1",
        "",
    );
    assert_eq!(bytes(&forward.json()["data"]), content[7..150_008]);
    let end = get(&f, &token, "files/lines?path=latest.log&limit=1", "");
    assert_eq!(bytes(&end.json()["data"]), "last π".as_bytes());
    for path in [
        "files/tail?path=latest.log&lines=1001",
        "files/lines?path=latest.log&from_byte=2",
        "files/lines?path=latest.log&limit=0",
    ] {
        management_reply_error(&get(&f, &token, path, ""), 400, "invalid_request");
    }
    std::fs::write(f.temp.path().join("game/binary.log"), [255, 0, b'\n']).unwrap();
    let binary = get(&f, &token, "files/tail?path=binary.log&lines=1", "");
    assert_eq!(binary.json()["data"]["encoding"], "binary");
    assert_eq!(bytes(&binary.json()["data"]), [255, 0, b'\n']);
}
#[test]
fn tls_follow_appends_truncates_replaces_and_shares_console_caps() {
    let f = Fixture::new("minecraft", 8, 4096, 5);
    let (token, device) = owner_token(&f);
    let disk = f.temp.path().join("game/latest.log");
    std::fs::write(&disk, b"initial\n").unwrap();
    let mut peer = follow(&f, &token, "path=latest.log&cursor=8").unwrap();
    let available = notice(&mut peer);
    assert_eq!(available["type"], "files.available");
    assert_eq!(available["cursor"], "8");
    std::fs::OpenOptions::new()
        .append(true)
        .open(&disk)
        .unwrap()
        .write_all(b"appended\n")
        .unwrap();
    let append = notice(&mut peer);
    assert_eq!(append["type"], "files.append");
    assert_eq!(bytes(&append), b"appended\n");
    assert_eq!(append["from_byte"], "8");
    std::fs::write(&disk, b"new\n").unwrap();
    assert_eq!(notice(&mut peer)["type"], "files.rotated");
    assert_eq!(bytes(&notice(&mut peer)), b"new\n");
    std::fs::write(f.temp.path().join("game/replacement.log"), b"replaced\n").unwrap();
    std::fs::rename(&disk, f.temp.path().join("game/old.log")).unwrap();
    std::fs::rename(f.temp.path().join("game/replacement.log"), &disk).unwrap();
    assert_eq!(notice(&mut peer)["type"], "files.rotated");
    assert_eq!(bytes(&notice(&mut peer)), b"replaced\n");
    let mut console = realtime::console_websocket(&f, &token).unwrap();
    assert!(matches!(console.read().unwrap(), Message::Text(_)));
    assert_eq!(follow(&f, &token, "path=latest.log").unwrap_err(), 503);
    assert_eq!(realtime::console_websocket(&f, &token).unwrap_err(), 503);
    f.host().management().revoke_local(&device).unwrap();
    assert!(matches!(peer.read().unwrap(), Message::Close(_)));
}
#[test]
fn tls_follow_rate_frames_and_default_end_cursor_are_bounded() {
    let f = Fixture::new("minecraft", 8, 4096, 5);
    let (token, _) = owner_token(&f);
    let disk = f.temp.path().join("game/latest.log");
    std::fs::write(&disk, b"existing").unwrap();
    let mut peer = follow(&f, &token, "path=latest.log").unwrap();
    assert_eq!(notice(&mut peer)["cursor"], "8");
    let data = vec![b'z'; 64 * 1024];
    std::fs::OpenOptions::new()
        .append(true)
        .open(&disk)
        .unwrap()
        .write_all(&data)
        .unwrap();
    let start = Instant::now();
    let mut actual = Vec::new();
    for _ in 0..4 {
        let next = notice(&mut peer);
        assert_eq!(next["type"], "files.append");
        let chunk = bytes(&next);
        assert!(chunk.len() <= 16 * 1024);
        actual.extend(chunk);
    }
    assert_eq!(actual, data);
    assert!(start.elapsed() >= Duration::from_millis(500));
}
#[test]
fn tls_new_file_routes_refuse_capability_and_protected_aliases() {
    let f = Fixture::new("minecraft", 8, 4096, 5);
    let pack = f.device_session();
    let (token, _) = owner_token(&f);
    std::fs::write(f.temp.path().join("game/whitelist.json"), b"private").unwrap();
    std::fs::hard_link(
        f.temp.path().join("game/whitelist.json"),
        f.temp.path().join("game/alias.log"),
    )
    .unwrap();
    for name in ["whitelist.json", "alias.log"] {
        for (path, headers) in [
            (format!("files/content?path={name}"), "Range: bytes=0-1\r\n"),
            (format!("files/tail?path={name}"), ""),
            (format!("files/lines?path={name}"), ""),
        ] {
            management_reply_error(&get(&f, &token, &path, headers), 400, "invalid_request");
            management_reply_error(&get(&f, &pack, &path, headers), 403, "management_forbidden");
        }
        assert_eq!(
            follow(&f, &token, &format!("path={name}")).unwrap_err(),
            400
        );
        assert_eq!(follow(&f, &pack, &format!("path={name}")).unwrap_err(), 403);
    }
}
fn gzip(bytes: &[u8]) -> Vec<u8> {
    use std::io::Write;
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(bytes).unwrap();
    encoder.finish().unwrap()
}
#[test]
fn tls_gzip_logs_serve_decompressed_ranges_and_pages_only_on_request() {
    let f = Fixture::new("minecraft", 8, 4096, 5);
    let (token, _) = owner_token(&f);
    let text: Vec<u8> = (0..60_000)
        .flat_map(|i| format!("[09:00:00] [Server thread/WARN]: entry {i}\n").into_bytes())
        .collect();
    let archive = gzip(&text);
    std::fs::write(f.temp.path().join("game/logs.log.gz"), &archive).unwrap();
    // Without the flag the archive bytes themselves are served, unchanged.
    let raw = get(
        &f,
        &token,
        "files/content?path=logs.log.gz",
        "Range: bytes=0-1\r\n",
    );
    assert_eq!(raw.status, 206);
    assert_eq!(raw.body, [0x1f, 0x8b]);
    // With the flag, ranges address the decompressed text.
    let path = "files/content?path=logs.log.gz&decompress=1";
    let reply = get(&f, &token, path, "Range: bytes=-46\r\n");
    assert_eq!(reply.status, 206);
    assert_eq!(reply.body, text[text.len() - 46..]);
    let etag = reply.headers["etag"].clone();
    assert!(etag.starts_with("\"gz-"));
    assert_eq!(reply.headers["x-vesta-file-size"], text.len().to_string());
    // A decompressed view is only read in bounded ranges, and stale If-Range is 412.
    management_reply_error(&get(&f, &token, path, ""), 400, "range_required");
    management_reply_error(
        &get(
            &f,
            &token,
            path,
            "Range: bytes=0-9\r\nIf-Range: \"stale\"\r\n",
        ),
        412,
        "precondition_failed",
    );
    let tail = get(
        &f,
        &token,
        "files/tail?path=logs.log.gz&lines=1&decompress=1",
        "",
    );
    assert_eq!(tail.status, 200);
    assert_eq!(
        bytes(&tail.json()["data"]),
        b"[09:00:00] [Server thread/WARN]: entry 59999\n"
    );
    // Corrupt archives, non-archives and bad flag values refuse.
    std::fs::write(
        f.temp.path().join("game/broken.log.gz"),
        &archive[..archive.len() / 3],
    )
    .unwrap();
    management_reply_error(
        &get(&f, &token, "files/tail?path=broken.log.gz&decompress=1", ""),
        422,
        "archive_corrupt",
    );
    std::fs::write(f.temp.path().join("game/plain.log"), b"plain\n").unwrap();
    management_reply_error(
        &get(&f, &token, "files/tail?path=plain.log&decompress=1", ""),
        400,
        "not_compressed",
    );
    management_reply_error(
        &get(&f, &token, "files/tail?path=logs.log.gz&decompress=yes", ""),
        400,
        "invalid_request",
    );
    // Follow has no decompressed mode.
    assert_eq!(
        follow(&f, &token, "path=logs.log.gz&decompress=1").err(),
        Some(400)
    );
}
#[test]
fn tls_downloads_stream_whole_files_with_names_resume_and_refusals() {
    let f = Fixture::new("minecraft", 8, 4096, 5);
    let (token, _) = owner_token(&f);
    let content: Vec<_> = (0..9 * 1024 * 1024 + 5).map(|i| (i % 253) as u8).collect();
    std::fs::write(f.temp.path().join("game/world-backup.bin"), &content).unwrap();
    let path = "files/download?path=world-backup.bin";
    let whole = get(&f, &token, path, "");
    assert_eq!(whole.status, 200);
    assert_eq!(
        whole.body.len(),
        content.len(),
        "no 1 MiB or 4 MiB cap on downloads"
    );
    assert_eq!(whole.body, content);
    assert_eq!(
        whole.headers["content-disposition"],
        "attachment; filename=\"world-backup.bin\""
    );
    let etag = whole.headers["etag"].clone();
    // Resume from an offset when the file is unchanged.
    let resumed = get(
        &f,
        &token,
        path,
        &format!("Range: bytes=7000000-\r\nIf-Range: {etag}\r\n"),
    );
    assert_eq!(resumed.status, 206);
    assert_eq!(resumed.body, content[7_000_000..]);
    assert_eq!(
        resumed.headers["content-range"],
        format!("bytes 7000000-{}/{}", content.len() - 1, content.len())
    );
    // A stale validator sends the whole current file again instead of mixing versions.
    let restarted = get(
        &f,
        &token,
        path,
        "Range: bytes=7000000-\r\nIf-Range: \"stale\"\r\n",
    );
    assert_eq!(restarted.status, 200);
    assert_eq!(restarted.body.len(), content.len());
    management_reply_error(
        &get(
            &f,
            &token,
            path,
            &format!("Range: bytes={}-\r\n", content.len()),
        ),
        416,
        "range_not_satisfiable",
    );
    management_reply_error(
        &get(&f, &token, path, "Range: bytes=0-10\r\n"),
        416,
        "range_not_satisfiable",
    );
    // Decompressed download of a rotated log drops the .gz from the file name.
    let text = b"[10:00:00] [Server thread/INFO]: hello\n".repeat(1000);
    std::fs::write(f.temp.path().join("game/2026-10-09-1.log.gz"), gzip(&text)).unwrap();
    let unzipped = get(
        &f,
        &token,
        "files/download?path=2026-10-09-1.log.gz&decompress=1",
        "",
    );
    assert_eq!(unzipped.status, 200);
    assert_eq!(unzipped.body, text);
    assert_eq!(
        unzipped.headers["content-disposition"],
        "attachment; filename=\"2026-10-09-1.log\""
    );
    // Protected documents stay excluded, and a role without files.read is refused.
    std::fs::write(f.temp.path().join("game/ops.json"), b"[]").unwrap();
    assert_ne!(
        get(&f, &token, "files/download?path=ops.json", "").status,
        200
    );
    let (device, key) = enroll_management(&f, vesta_control::identity::BuiltinRole::PackUser);
    let pack_only = management_session(&f, &device, &key);
    assert_eq!(get(&f, &pack_only, path, "").status, 403);
}
