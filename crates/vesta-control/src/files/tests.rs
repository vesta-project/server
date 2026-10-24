use super::*;
use crate::identity::{DeviceId, PrincipalId};
use rusqlite::params;
use std::{fs, path::PathBuf, sync::Arc};
use tempfile::TempDir;
use vesta_state::{Clock, StateDirectory};

struct FixedClock;
impl Clock for FixedClock {
    fn millis(&self) -> i64 {
        1_791_115_200_000
    }
}
struct Fixture {
    _temp: TempDir,
    game: PathBuf,
    db: StateDatabase,
    owner: Subject,
    browser: FileBrowser,
}
impl Fixture {
    fn open() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let game = temp.path().join("game");
        fs::create_dir(&game).unwrap();
        let directory =
            StateDirectory::new(temp.path().join("private-state"), Some(&game)).unwrap();
        let db = StateDatabase::open(directory, Arc::new(FixedClock)).unwrap();
        let owner = seed(&db, true);
        let browser = FileBrowser::open(db.clone(), &game, &[]).unwrap();
        Self {
            _temp: temp,
            game,
            db,
            owner,
            browser,
        }
    }
    fn write(&self, path: &str, bytes: &[u8]) {
        let path = self.game.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }
    fn grant(&self, subject: &Subject, capability: Capability, scope: &str, effect: &str) {
        self.db.write(|c| {
            c.execute("INSERT INTO principal_overrides (id,principal_id,capability,scope,effect,granted_by,created_at) VALUES (?,?,?,?,?,'fixture',?)",
                params![uuid::Uuid::new_v4().to_string(), subject.principal.as_str(), capability.id(), scope, effect, self.db.clock().millis()])?;
            Ok(())
        }).unwrap();
    }
    fn read(&self, path: &str, limit: usize) -> Result<FileRead> {
        self.browser
            .read(&self.owner, &RelativePath::parse(path).unwrap(), limit)
    }
}
fn seed(db: &StateDatabase, owner: bool) -> Subject {
    let principal = PrincipalId::parse(&uuid::Uuid::new_v4().to_string()).unwrap();
    let device = DeviceId::parse(&uuid::Uuid::new_v4().to_string()).unwrap();
    db.write(|c| {
        c.execute("INSERT INTO principals (id,server_id,display_name,status,created_at,updated_at) VALUES (?,?,'Fixture','active',?,?)",
            params![principal.as_str(), db.server_id(), db.clock().millis(), db.clock().millis()])?;
        c.execute("INSERT INTO devices (id,server_id,principal_id,label,public_key_sha256,created_at) VALUES (?,?,?,'Fixture',?,?)",
            params![device.as_str(), db.server_id(), principal.as_str(), hex::encode(Sha256::digest(device.as_str().as_bytes())), db.clock().millis()])?;
        if owner {
            c.execute("INSERT INTO role_assignments (id,principal_id,role_id,granted_by,created_at) VALUES (?,?,'owner','fixture',?)",
                params![uuid::Uuid::new_v4().to_string(), principal.as_str(), db.clock().millis()])?;
        }
        Ok(())
    }).unwrap();
    Subject { principal, device }
}
fn refused<T>(result: Result<T>, expected: &str) {
    match result {
        Ok(_) => panic!("must refuse with {expected}"),
        Err(error) => assert_eq!(error.code(), expected),
    }
}

#[test]
fn portable_paths_refuse_ambiguous_spelling_and_devices_without_echo() {
    for text in [
        "",
        "/x",
        "\\x",
        "a\\b",
        "a/",
        "a//b",
        ".",
        "..",
        "a/../b",
        "a/./b",
        "C:/x",
        "//host/x",
        "a:b",
        " a",
        "a.",
        "a ",
        "a\n",
        "a\u{7f}",
        "résumé",
        "COM0.txt",
        "LPT0",
        "con",
        "nul.txt",
        "lpt9.cfg",
        "COM¹.txt",
        "conin$",
        "a*",
        "a?",
        "a|",
        "a\"",
        "a<b",
    ] {
        let error = RelativePath::parse(text).unwrap_err();
        assert_eq!(error.code(), "file_path_invalid", "invalid portable name");
        assert_eq!(error.to_string(), "File operation failed");
        assert!(error.source().is_none());
    }
    for text in [
        "mods/example+1.jar",
        "config/a b.json",
        "notes.txt",
        "a/.hidden",
        "a-b_c",
    ] {
        assert_eq!(RelativePath::parse(text).unwrap().as_str(), text);
    }
}
#[test]
fn protected_names_are_denied_case_insensitively_at_every_depth() {
    for name in path::PROTECTED_NAMES {
        for text in [
            name.to_string(),
            name.to_uppercase(),
            format!("mods/{name}/config.json"),
        ] {
            refused(RelativePath::parse(&text), "file_path_protected");
        }
    }
}
#[test]
fn path_bounds_are_inclusive_and_debug_is_redacted() {
    let segment = "x".repeat(MAX_COMPONENT_BYTES);
    assert!(RelativePath::parse(&segment).is_ok());
    refused(
        RelativePath::parse(&"x".repeat(MAX_COMPONENT_BYTES + 1)),
        "file_path_invalid",
    );
    let path = std::iter::repeat_n(segment.as_str(), 16)
        .collect::<Vec<_>>()
        .join("/")
        + "/";
    assert_eq!(path.len(), MAX_PATH_BYTES);
    let exact = path[..path.len() - 2].to_owned() + "/x";
    assert_eq!(exact.len(), MAX_PATH_BYTES);
    let validated = RelativePath::parse(&exact).unwrap();
    assert_eq!(format!("{validated:?}"), "RelativePath[<redacted>]");
    refused(RelativePath::parse(&(exact + "x")), "file_path_invalid");
}
#[test]
fn ordinary_binary_reads_hash_the_returned_bytes_and_never_expose_native_paths() {
    let f = Fixture::open();
    let bytes = b"owned\0fixture\xffsecret";
    f.write("config/payload.bin", bytes);
    let read = f.read("config/payload.bin", bytes.len()).unwrap();
    assert_eq!(read.bytes(), bytes);
    assert_eq!(read.sha256(), hex::encode(Sha256::digest(bytes)));
    assert!(read.modified_at().is_some());
    assert!(!format!("{read:?}").contains("secret"));
    assert_eq!(format!("{:?}", f.browser), "FileBrowser[<redacted>]");
}
#[test]
fn read_limits_missing_files_and_directories_refuse() {
    let f = Fixture::open();
    f.write("payload", b"four");
    refused(f.read("payload", 0), "file_limit_invalid");
    refused(f.read("payload", MAX_READ_BYTES + 1), "file_limit_invalid");
    refused(f.read("payload", 3), "file_too_large");
    assert_eq!(f.read("payload", 4).unwrap().bytes(), b"four");
    refused(f.read("missing", 10), "file_not_found");
    fs::create_dir(f.game.join("directory")).unwrap();
    refused(f.read("directory", 10), "file_not_regular");
    f.write("empty", b"");
    assert!(f.read("empty", 1).unwrap().bytes().is_empty());
    f.write("maximum", &vec![42; MAX_READ_BYTES]);
    assert_eq!(
        f.read("maximum", MAX_READ_BYTES).unwrap().bytes().len(),
        MAX_READ_BYTES
    );
}
#[test]
fn lists_are_bounded_sorted_and_hide_protected_or_nonportable_entries() {
    let f = Fixture::open();
    f.write("b.txt", b"abc");
    f.write("A.txt", b"x");
    f.write(".vesta/secret", b"hidden");
    f.write("wrapper-config.toml", b"hidden");
    f.write("unicode-λ", b"hidden");
    fs::create_dir(f.game.join("dir")).unwrap();
    let listing = f.browser.list(&f.owner, None).unwrap();
    assert_eq!(
        listing
            .entries
            .iter()
            .map(|entry| entry.name.as_str())
            .collect::<Vec<_>>(),
        ["A.txt", "b.txt", "dir"]
    );
    assert_eq!(listing.entries[1].size, 3);
    assert_eq!(listing.entries[2].kind, EntryKind::Directory);
    assert_eq!(listing.entries[2].size, 0);
    refused(
        f.browser
            .list(&f.owner, Some(&RelativePath::parse("b.txt").unwrap())),
        "file_not_directory",
    );
}
#[test]
fn returned_entry_limit_fails_instead_of_silently_truncating() {
    let f = Fixture::open();
    for index in 0..=MAX_DIRECTORY_ENTRIES {
        f.write(&format!("entry-{index:04}"), b"");
    }
    refused(f.browser.list(&f.owner, None), "file_too_large");
}
#[test]
fn hidden_entries_also_consume_the_scan_budget() {
    let f = Fixture::open();
    for index in 0..=MAX_DIRECTORY_SCAN {
        f.write(&format!("λ-{index:04}"), b"");
    }
    refused(f.browser.list(&f.owner, None), "file_too_large");
}
#[test]
fn host_protected_paths_include_missing_secrets_and_case_variants() {
    let f = Fixture::open();
    let secret = f.game.join("secret.txt");
    let folder = f.game.join("credentials");
    let browser =
        FileBrowser::open(f.db.clone(), &f.game, &[secret.as_path(), folder.as_path()]).unwrap();
    for path in [
        "secret.txt",
        "credentials/token",
        "credentials/missing/deeper",
    ] {
        let path = RelativePath::parse(path).unwrap();
        refused(browser.read(&f.owner, &path, 100), "file_path_protected");
        refused(browser.list(&f.owner, Some(&path)), "file_path_protected");
    }
    #[cfg(windows)]
    refused(
        browser.read(&f.owner, &RelativePath::parse("SECRET.TXT").unwrap(), 100),
        "file_path_protected",
    );
    f.write("secret.txt", b"secret");
    f.write("credentials/token", b"secret");
    for path in ["secret.txt", "credentials/token"] {
        refused(
            browser.read(&f.owner, &RelativePath::parse(path).unwrap(), 100),
            "file_path_protected",
        );
    }
    assert!(browser.list(&f.owner, None).unwrap().entries.is_empty());
    #[cfg(windows)]
    refused(
        browser.read(&f.owner, &RelativePath::parse("SECRET.TXT").unwrap(), 100),
        "file_path_protected",
    );
}
#[cfg(any(windows, unix))]
#[test]
fn missing_protected_suffix_preserves_existing_directory_aliases() {
    let f = Fixture::open();
    let real = f.game.join("credentials-real");
    fs::create_dir(&real).unwrap();
    let alias = f.game.join("credentials-alias");
    #[cfg(windows)]
    {
        let status = std::process::Command::new("cmd.exe")
            .args(["/d", "/c", "mklink", "/J"])
            .arg(&alias)
            .arg(&real)
            .status()
            .unwrap();
        assert!(status.success(), "create owned protected alias fixture");
    }
    #[cfg(unix)]
    std::os::unix::fs::symlink(&real, &alias).unwrap();
    let secret = alias.join("missing/token");
    let browser = FileBrowser::open(f.db.clone(), &f.game, &[&secret]).unwrap();
    for spelling in [
        "credentials-alias/missing/token",
        "credentials-real/missing/token",
    ] {
        let path = RelativePath::parse(spelling).unwrap();
        refused(browser.read(&f.owner, &path, 100), "file_path_protected");
        refused(browser.list(&f.owner, Some(&path)), "file_path_protected");
    }
    f.write("credentials-real/missing/token", b"protected bytes");
    f.write("credentials-real/missing/public", b"public bytes");
    let path = RelativePath::parse("credentials-real/missing/token").unwrap();
    refused(browser.read(&f.owner, &path, 100), "file_path_protected");
    assert_eq!(
        browser
            .list(
                &f.owner,
                Some(&RelativePath::parse("credentials-real/missing").unwrap()),
            )
            .unwrap()
            .entries
            .iter()
            .map(|entry| entry.name.as_str())
            .collect::<Vec<_>>(),
        ["public"]
    );
    assert_eq!(
        browser
            .read(
                &f.owner,
                &RelativePath::parse("credentials-real/missing/public").unwrap(),
                100,
            )
            .unwrap()
            .bytes(),
        b"public bytes"
    );
    #[cfg(windows)]
    fs::remove_dir(&alias).unwrap();
    #[cfg(unix)]
    fs::remove_file(&alias).unwrap();
    assert_eq!(
        fs::read(real.join("missing/token")).unwrap(),
        b"protected bytes"
    );
}
#[cfg(unix)]
#[test]
fn dangling_protected_ancestor_refuses_instead_of_losing_its_alias() {
    let f = Fixture::open();
    let alias = f.game.join("dangling");
    std::os::unix::fs::symlink(f.game.join("missing-target"), &alias).unwrap();
    refused(
        FileBrowser::open(f.db.clone(), &f.game, &[&alias.join("token")]),
        "file_path_linked",
    );
}
#[cfg(windows)]
#[test]
fn ambiguous_protected_spelling_refuses_before_native_normalization() {
    let f = Fixture::open();
    f.write("existing", b"protected bytes");
    for spelling in [
        "secret.",
        "secret ",
        "secret:stream",
        "missing./token",
        "existing.",
        "existing ",
        "existing:stream",
    ] {
        refused(
            FileBrowser::open(f.db.clone(), &f.game, &[&f.game.join(spelling)]),
            "file_root_invalid",
        );
    }
}
#[test]
fn replacing_a_plain_root_uses_the_configured_path_not_a_pinned_directory_id() {
    let f = Fixture::open();
    f.write("old", b"original bytes");
    let old = f._temp.path().join("original-game");
    fs::rename(&f.game, &old).unwrap();
    fs::create_dir(&f.game).unwrap();
    f.write("new", b"replacement bytes");
    assert_eq!(f.read("new", 100).unwrap().bytes(), b"replacement bytes");
    refused(f.read("old", 100), "file_not_found");
    assert_eq!(fs::read(old.join("old")).unwrap(), b"original bytes");
}
#[test]
fn state_root_is_automatically_protected_and_cannot_be_the_browser_root() {
    let f = Fixture::open();
    refused(
        FileBrowser::open(f.db.clone(), f.db.directory().path(), &[]),
        "file_path_protected",
    );
    let browser = FileBrowser::open(f.db.clone(), f._temp.path(), &[]).unwrap();
    assert_eq!(
        browser
            .list(&f.owner, None)
            .unwrap()
            .entries
            .iter()
            .map(|entry| entry.name.as_str())
            .collect::<Vec<_>>(),
        ["game"]
    );
    refused(
        browser.read(
            &f.owner,
            &RelativePath::parse("private-state/vesta-state.db").unwrap(),
            100,
        ),
        "file_path_protected",
    );
}
#[test]
fn configuration_and_path_possession_do_not_create_authority_or_reveal_existence() {
    let f = Fixture::open();
    let subject = seed(&f.db, false);
    f.write("payload", b"secret");
    refused(
        f.browser
            .read(&subject, &RelativePath::parse("payload").unwrap(), 100),
        "forbidden",
    );
    refused(
        f.browser
            .read(&subject, &RelativePath::parse("missing").unwrap(), 100),
        "forbidden",
    );
    refused(f.browser.list(&subject, None), "forbidden");
}
#[test]
fn scoped_reads_and_denies_reload_policy_on_every_call() {
    let f = Fixture::open();
    let subject = seed(&f.db, false);
    f.write("config/allowed", b"allowed");
    f.write("mods/other", b"other");
    f.grant(&subject, Capability::FilesRead, "config/**", "allow");
    let path = RelativePath::parse("config/allowed").unwrap();
    assert_eq!(
        f.browser.read(&subject, &path, 100).unwrap().bytes(),
        b"allowed"
    );
    refused(
        f.browser
            .read(&subject, &RelativePath::parse("mods/other").unwrap(), 100),
        "forbidden",
    );
    f.grant(&subject, Capability::FilesRead, "config/allowed", "deny");
    refused(f.browser.read(&subject, &path, 100), "forbidden");
}
#[test]
fn listing_child_denies_and_device_revocation_are_observed() {
    let f = Fixture::open();
    f.write("allowed", b"x");
    f.write("hidden", b"x");
    f.grant(&f.owner, Capability::FilesList, "hidden", "deny");
    assert_eq!(
        f.browser
            .list(&f.owner, None)
            .unwrap()
            .entries
            .iter()
            .map(|entry| entry.name.as_str())
            .collect::<Vec<_>>(),
        ["allowed"]
    );
    f.db.write(|c| {
        c.execute(
            "UPDATE devices SET revoked_at=? WHERE id=?",
            params![f.db.clock().millis(), f.owner.device.as_str()],
        )?;
        Ok(())
    })
    .unwrap();
    refused(f.browser.list(&f.owner, None), "forbidden");
    refused(f.read("allowed", 100), "forbidden");
}
#[test]
fn hard_link_aliases_cannot_read_or_list_other_files() {
    let f = Fixture::open();
    let secret = f._temp.path().join("secret");
    fs::write(&secret, b"outside secret").unwrap();
    fs::hard_link(&secret, f.game.join("alias")).unwrap();
    refused(f.read("alias", 100), "file_path_linked");
    assert!(f.browser.list(&f.owner, None).unwrap().entries.is_empty());
    assert_eq!(fs::read(&secret).unwrap(), b"outside secret");
}
#[cfg(windows)]
#[test]
fn directory_junctions_are_refused_without_accessing_the_target() {
    let f = Fixture::open();
    let outside = f._temp.path().join("outside");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("secret"), b"outside secret").unwrap();
    let junction = f.game.join("junction");
    let status = std::process::Command::new("cmd.exe")
        .args(["/d", "/c", "mklink", "/J"])
        .arg(&junction)
        .arg(&outside)
        .status()
        .unwrap();
    assert!(status.success(), "create owned junction fixture");
    refused(f.read("junction/secret", 100), "file_path_linked");
    assert!(f.browser.list(&f.owner, None).unwrap().entries.is_empty());
    refused(
        FileBrowser::open(f.db.clone(), &junction, &[]),
        "file_path_linked",
    );
    fs::remove_dir(junction).unwrap();
    assert_eq!(fs::read(outside.join("secret")).unwrap(), b"outside secret");
}
#[cfg(windows)]
#[test]
fn actual_disk_case_is_authorized_not_just_the_requested_alias() {
    let f = Fixture::open();
    f.write("Actual.txt", b"secret");
    let subject = seed(&f.db, false);
    f.grant(&subject, Capability::FilesRead, "actual.txt", "allow");
    refused(
        f.browser
            .read(&subject, &RelativePath::parse("actual.txt").unwrap(), 100),
        "forbidden",
    );
    f.grant(&subject, Capability::FilesRead, "Actual.txt", "allow");
    assert_eq!(
        f.browser
            .read(&subject, &RelativePath::parse("Actual.txt").unwrap(), 100)
            .unwrap()
            .bytes(),
        b"secret"
    );
}
#[cfg(unix)]
#[test]
fn file_and_directory_symlinks_are_refused() {
    use std::os::unix::fs::symlink;
    let f = Fixture::open();
    let outside = f._temp.path().join("outside");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("secret"), b"secret").unwrap();
    symlink(&outside, f.game.join("directory-link")).unwrap();
    symlink(outside.join("secret"), f.game.join("file-link")).unwrap();
    refused(f.read("directory-link/secret", 100), "file_path_linked");
    refused(f.read("file-link", 100), "file_path_linked");
    assert!(f.browser.list(&f.owner, None).unwrap().entries.is_empty());
}

#[test]
fn growing_file_chunks_metadata_and_live_authority_are_bounded() {
    let f = Fixture::open();
    let bytes = vec![b'a'; 5 * 1024 * 1024 + 7];
    f.write("latest.log", &bytes);
    let path = RelativePath::parse("latest.log").unwrap();
    let mut stream = f.browser.open_stream(&f.owner, &path).unwrap();
    assert_eq!(stream.size(), bytes.len() as u64);
    assert!(!stream.etag().starts_with("W/"));
    let etag = stream.etag().to_owned();
    let identity = stream.identity().to_owned();
    let mut offset = 0;
    let mut chunks = 0;
    while offset < stream.size() {
        let chunk = stream.read_chunk(offset, FILE_CHUNK_BYTES).unwrap();
        assert!(chunk.len() <= FILE_CHUNK_BYTES);
        assert_eq!(chunk, bytes[offset as usize..offset as usize + chunk.len()]);
        offset += chunk.len() as u64;
        chunks += 1;
    }
    assert_eq!(chunks, 81);
    assert!(stream.unchanged().unwrap());
    use std::io::Write;
    fs::OpenOptions::new()
        .append(true)
        .open(f.game.join("latest.log"))
        .unwrap()
        .write_all(b"tail")
        .unwrap();
    assert!(!stream.unchanged().unwrap());
    let changed = f.browser.open_stream(&f.owner, &path).unwrap();
    assert_ne!(changed.etag(), etag);
    assert_eq!(changed.identity(), identity);
    let denied = seed(&f.db, false);
    refused(f.browser.open_stream(&denied, &path), "forbidden");
    f.grant(&f.owner, Capability::FilesRead, "latest.log", "deny");
    refused(f.browser.open_stream(&f.owner, &path), "forbidden");
}
#[test]
fn text_pages_tail_long_lines_and_bidirectional_cursors_preserve_bytes() {
    let f = Fixture::open();
    let bytes = [b"first\r\n".as_slice(), &vec![b'x'; 150_000], b"\nlast"].concat();
    f.write("latest.log", &bytes);
    let path = RelativePath::parse("latest.log").unwrap();
    let mut file = f.browser.open_stream(&f.owner, &path).unwrap();
    let tail = file.lines(file.size(), 2, true).unwrap();
    assert_eq!(tail.start, 7);
    assert_eq!(tail.end, file.size());
    assert_eq!(tail.bytes, bytes[7..]);
    let previous = file.lines(tail.start, 1, true).unwrap();
    assert_eq!(previous.bytes, b"first\r\n");
    let forward = file.lines(previous.end, 1, false).unwrap();
    assert_eq!(forward.bytes, bytes[7..150_008]);
    let last = file.lines(forward.end, 1, false).unwrap();
    assert_eq!(last.bytes, b"last");
    assert!(file.lines(last.end, 1, false).unwrap().bytes.is_empty());
    refused(file.lines(2, 1, false), "file_limit_invalid");
    refused(file.lines(2, 1, true), "file_limit_invalid");
    refused(file.lines(0, 1001, false), "file_limit_invalid");
    f.write("trailing.log", b"a\nb\nc\n");
    let mut file = f
        .browser
        .open_stream(&f.owner, &RelativePath::parse("trailing.log").unwrap())
        .unwrap();
    assert_eq!(file.lines(file.size(), 2, true).unwrap().bytes, b"b\nc\n");
    f.write("empty.log", b"");
    let mut file = f
        .browser
        .open_stream(&f.owner, &RelativePath::parse("empty.log").unwrap())
        .unwrap();
    assert!(file.lines(0, 1, true).unwrap().bytes.is_empty());
}
#[test]
fn stream_documents_and_aliases_are_excluded_and_encoding_is_not_transformed() {
    let f = Fixture::open();
    for name in [
        "server.properties",
        "whitelist.json",
        "ops.json",
        "banned-players.json",
        "banned-ips.json",
    ] {
        f.write(name, b"private");
        refused(
            f.browser
                .open_stream(&f.owner, &RelativePath::parse(name).unwrap()),
            "file_path_protected",
        );
    }
    fs::hard_link(f.game.join("ops.json"), f.game.join("alias.log")).unwrap();
    refused(
        f.browser
            .open_stream(&f.owner, &RelativePath::parse("alias.log").unwrap()),
        "file_path_linked",
    );
    assert_eq!(encoding_hint("Unicode π\n".as_bytes()), "utf-8");
    assert_eq!(encoding_hint(&[0, 255]), "binary");
    f.write("binary.log", &[0, 255, b'\n']);
    let mut file = f
        .browser
        .open_stream(&f.owner, &RelativePath::parse("binary.log").unwrap())
        .unwrap();
    let page = file.lines(file.size(), 1, true).unwrap();
    assert_eq!(page.encoding, "binary");
    assert_eq!(page.bytes, [0, 255, b'\n']);
}

fn gzip(bytes: &[u8]) -> Vec<u8> {
    use std::io::Write;
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(bytes).unwrap();
    encoder.finish().unwrap()
}
fn spooled_logs(f: &Fixture) -> usize {
    f.db.directory()
        .spool()
        .unwrap()
        .list(256)
        .unwrap()
        .iter()
        .filter(|name| name.as_str().ends_with(".log"))
        .count()
}

#[test]
fn rotated_gzip_logs_read_as_a_reused_bounded_decompressed_view() {
    let f = Fixture::open();
    let text: Vec<u8> = (0..200_000)
        .flat_map(|i| format!("[12:00:00] [Server thread/INFO]: line {i}\n").into_bytes())
        .collect();
    assert!(text.len() > 5 * 1024 * 1024);
    f.write("2026-10-10-1.log.gz", &gzip(&text));
    let path = RelativePath::parse("2026-10-10-1.log.gz").unwrap();
    let mut view = f.browser.open_decompressed(&f.owner, &path).unwrap();
    assert_eq!(view.size(), text.len() as u64);
    assert!(view.etag().starts_with("\"gz-"));
    let raw = f.browser.open_stream(&f.owner, &path).unwrap();
    assert_ne!(
        raw.etag(),
        view.etag(),
        "decompressed view has its own validator"
    );
    let start = 3 * 1024 * 1024;
    assert_eq!(
        view.read_chunk(start, FILE_CHUNK_BYTES).unwrap(),
        text[start as usize..start as usize + FILE_CHUNK_BYTES]
    );
    let tail = view.lines(view.size(), 2, true).unwrap();
    assert_eq!(
        tail.bytes,
        b"[12:00:00] [Server thread/INFO]: line 199998\n[12:00:00] [Server thread/INFO]: line 199999\n"
    );
    // The second open reuses the cached view instead of decompressing again.
    let again = f.browser.open_decompressed(&f.owner, &path).unwrap();
    assert_eq!(again.etag(), view.etag());
    assert_eq!(spooled_logs(&f), 1);
    // Authority and protected-document checks are those of the archive itself.
    let denied = seed(&f.db, false);
    refused(f.browser.open_decompressed(&denied, &path), "forbidden");
}

#[test]
fn only_real_gzip_archives_with_a_gz_name_are_decompressed() {
    let f = Fixture::open();
    f.write("latest.log", b"plain text\n");
    refused(
        f.browser
            .open_decompressed(&f.owner, &RelativePath::parse("latest.log").unwrap()),
        "file_not_compressed",
    );
    // Gzip bytes under a non-.gz name are never decompressed.
    f.write("disguised.log", &gzip(b"hidden\n"));
    refused(
        f.browser
            .open_decompressed(&f.owner, &RelativePath::parse("disguised.log").unwrap()),
        "file_not_compressed",
    );
    // A .gz name without gzip magic is refused too.
    f.write("fake.log.gz", b"not gzip at all");
    refused(
        f.browser
            .open_decompressed(&f.owner, &RelativePath::parse("fake.log.gz").unwrap()),
        "file_not_compressed",
    );
    assert_eq!(spooled_logs(&f), 0);
}

#[test]
fn corrupt_truncated_and_bomb_archives_serve_nothing() {
    let f = Fixture::open();
    let archive = gzip(&b"line\n".repeat(50_000));
    f.write("truncated.log.gz", &archive[..archive.len() / 2]);
    refused(
        f.browser
            .open_decompressed(&f.owner, &RelativePath::parse("truncated.log.gz").unwrap()),
        "file_corrupt",
    );
    let mut flipped = archive.clone();
    let crc = flipped.len() - 8;
    flipped[crc] ^= 0xff;
    f.write("crc.log.gz", &flipped);
    refused(
        f.browser
            .open_decompressed(&f.owner, &RelativePath::parse("crc.log.gz").unwrap()),
        "file_corrupt",
    );
    // A tiny archive that expands far beyond the allowed ratio is refused.
    f.write("bomb.log.gz", &gzip(&vec![0; 4 * 1024 * 1024]));
    refused(
        f.browser
            .open_decompressed(&f.owner, &RelativePath::parse("bomb.log.gz").unwrap()),
        "file_too_large",
    );
    assert_eq!(spooled_logs(&f), 0, "failed views leave nothing cached");
}

#[test]
fn decompressed_views_expire_after_their_ttl_and_are_rebuilt() {
    let f = Fixture::open();
    f.write("old.log.gz", &gzip(b"first\nsecond\n"));
    let path = RelativePath::parse("old.log.gz").unwrap();
    f.browser.open_decompressed(&f.owner, &path).unwrap();
    let spool = f.db.directory().path().join("spool");
    let cached = fs::read_dir(&spool)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|p| p.extension().is_some_and(|e| e == "log"))
        .unwrap();
    let aged = std::time::SystemTime::now()
        - super::stream::SPOOL_VIEW_TTL
        - std::time::Duration::from_secs(60);
    fs::File::options()
        .write(true)
        .open(&cached)
        .unwrap()
        .set_modified(aged)
        .unwrap();
    let mut view = f.browser.open_decompressed(&f.owner, &path).unwrap();
    assert_eq!(view.read_chunk(0, 13).unwrap(), b"first\nsecond\n");
    let rebuilt = fs::metadata(&cached).unwrap().modified().unwrap();
    assert!(rebuilt > aged, "an expired view is rebuilt, not reused");
    assert_eq!(spooled_logs(&f), 1);
}

#[test]
fn downloads_cover_the_size_seen_at_start_and_stop_on_truncation() {
    let f = Fixture::open();
    f.write("latest.log", b"0123456789");
    let path = RelativePath::parse("latest.log").unwrap();
    let stream = f.browser.open_stream(&f.owner, &path).unwrap();
    use std::io::Write;
    fs::OpenOptions::new()
        .append(true)
        .open(f.game.join("latest.log"))
        .unwrap()
        .write_all(b"appended")
        .unwrap();
    assert!(
        stream.still_covers().unwrap(),
        "appends do not break a download"
    );
    assert!(
        !stream.unchanged().unwrap(),
        "but they do break an exact range"
    );
    fs::write(f.game.join("latest.log"), b"short").unwrap();
    assert!(
        !stream.still_covers().unwrap(),
        "truncation stops a download"
    );
}

#[test]
fn protected_raw_edits_bind_receipt_payload_actor_and_validator() {
    let f = Fixture::open();
    f.write("server.properties", b"online-mode=true\nmax-players=20\n");
    let files = f.browser.for_remote();
    let path = RelativePath::parse("server.properties").unwrap();
    let old = files
        .read_editable(&f.owner, &path, MAX_READ_BYTES)
        .unwrap();
    let bytes = b"online-mode=false\nmax-players=24\n";
    let pending = files
        .write_protected(&f.owner, &path, old.sha256(), bytes, None, "raw-test")
        .unwrap();
    assert_eq!(
        fs::read(f.game.join("server.properties")).unwrap(),
        old.bytes()
    );
    let token = pending["confirmation_token"].as_str().unwrap();
    assert!(
        files
            .write_protected(
                &f.owner,
                &path,
                old.sha256(),
                b"online-mode=true\n",
                Some(token),
                "raw-test"
            )
            .is_err()
    );
    let result = files
        .write_protected(
            &f.owner,
            &path,
            old.sha256(),
            bytes,
            Some(token),
            "raw-test",
        )
        .unwrap();
    assert_eq!(result["state"], "applied");
    assert_eq!(
        files
            .write_protected(
                &f.owner,
                &path,
                old.sha256(),
                bytes,
                Some(token),
                "raw-test"
            )
            .unwrap(),
        result
    );
    assert!(RelativePath::parse(".vesta/private").is_err());
    let outsider = seed(&f.db, false);
    assert!(
        files
            .read_editable(&outsider, &path, MAX_READ_BYTES)
            .is_err()
    );
    assert!(
        files
            .write_protected(
                &f.owner,
                &path,
                result["sha256"].as_str().unwrap(),
                b"max-players=0\n",
                None,
                "bad-properties"
            )
            .is_err()
    );
}
