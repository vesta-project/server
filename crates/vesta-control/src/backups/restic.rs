//! A narrow adapter over a trusted, locally installed restic executable.
//!
//! Vesta does not implement backup storage or cryptography: restic provides
//! content-defined deduplication, authenticated encryption and repository checks.
//! Every invocation is a literal argument vector (no shell) with a cleared environment
//! plus an allowlist; the repository password is passed only as a protected file path.
//! Output is bounded and parsed from restic's JSON mode; nothing from restic output is
//! echoed back to callers except validated identifiers and numbers.
use serde::Deserialize;
use std::{
    io::Read,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::mpsc,
    time::{Duration, Instant},
};

const MAX_STDOUT: usize = 8 * 1024 * 1024;
const MAX_STDERR: usize = 64 * 1024;
const POLL: Duration = Duration::from_millis(20);
pub const MAX_SNAPSHOTS: usize = 10_000;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EngineError {
    /// The executable could not be started.
    Unavailable,
    /// restic exited unsuccessfully (repository missing, wrong password, I/O ...).
    Failed,
    /// restic reported that it could not read some source files (exit code 3).
    Incomplete,
    /// The repository is locked by another restic process.
    Locked,
    TimedOut,
    /// Output was malformed or exceeded its bound.
    Protocol,
    Invalid,
}
impl EngineError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::Unavailable => "backup_engine_unavailable",
            Self::Failed => "backup_engine_failed",
            Self::Incomplete => "backup_source_incomplete",
            Self::Locked => "backup_repository_locked",
            Self::TimedOut => "backup_engine_timed_out",
            Self::Protocol => "backup_engine_protocol",
            Self::Invalid => "backup_invalid_request",
        }
    }
}
pub type Result<T> = std::result::Result<T, EngineError>;

/// Retention passed to `restic forget`; zero means "not used".
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Retention {
    pub keep_last: u32,
    pub keep_daily: u32,
    pub keep_weekly: u32,
    pub keep_monthly: u32,
}
impl Retention {
    pub fn is_empty(&self) -> bool {
        self.keep_last == 0
            && self.keep_daily == 0
            && self.keep_weekly == 0
            && self.keep_monthly == 0
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BackupSummary {
    pub snapshot_id: String,
    pub files_new: u64,
    pub files_changed: u64,
    pub files_unmodified: u64,
    pub data_added: u64,
    pub total_bytes_processed: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Snapshot {
    pub id: String,
    /// RFC 3339 time as reported by restic, length-bounded.
    pub time: String,
    pub tags: Vec<String>,
}

/// RFC 3339 timestamps as restic prints them; nothing else is accepted from output.
fn valid_time(time: &str) -> bool {
    let b = time.as_bytes();
    (20..=40).contains(&b.len())
        && b[4] == b'-'
        && b[7] == b'-'
        && b[10] == b'T'
        && b[13] == b':'
        && b[16] == b':'
        && b.iter()
            .all(|c| c.is_ascii_digit() || b"-T:.+Z".contains(c))
}

/// Restore paths are passed to restic as include patterns: refuse pattern syntax so
/// a requested path always means exactly that path.
pub fn literal_path(path: &str) -> bool {
    !path
        .bytes()
        .any(|b| matches!(b, b'*' | b'?' | b'[' | b']' | b'\\' | b'{' | b'}'))
}

/// Validated restic snapshot identifier (short or full lowercase hex).
pub fn valid_snapshot_id(id: &str) -> bool {
    (8..=64).contains(&id.len()) && id.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// Tags are short fixed vocabulary built by Vesta (`vesta`, `profile=<name>`).
fn valid_tag(tag: &str) -> bool {
    !tag.is_empty()
        && tag.len() <= 80
        && tag
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'=' | b'-' | b'_' | b'.'))
}

pub struct ResticEngine {
    executable: PathBuf,
    repository: PathBuf,
    password_file: PathBuf,
    cache_dir: PathBuf,
    timeout: Duration,
}
impl std::fmt::Debug for ResticEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ResticEngine[<redacted>]")
    }
}

struct Output {
    stdout: Vec<u8>,
}

impl ResticEngine {
    /// All paths are trusted host configuration, validated by the caller.
    pub fn new(
        executable: PathBuf,
        repository: PathBuf,
        password_file: PathBuf,
        cache_dir: PathBuf,
        timeout: Duration,
    ) -> Self {
        Self {
            executable,
            repository,
            password_file,
            cache_dir,
            timeout,
        }
    }

    fn run(&self, args: &[&std::ffi::OsStr], cwd: Option<&Path>) -> Result<Output> {
        let mut command = Command::new(&self.executable);
        command
            .arg("--json")
            .args(args)
            .env_clear()
            .env("RESTIC_REPOSITORY", &self.repository)
            .env("RESTIC_PASSWORD_FILE", &self.password_file)
            .env("RESTIC_CACHE_DIR", &self.cache_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        // Minimal platform environment restic needs to run; nothing else is inherited.
        for name in ["SystemRoot", "TEMP", "TMP", "HOME", "LOCALAPPDATA"] {
            if let Some(value) = std::env::var_os(name) {
                command.env(name, value);
            }
        }
        if let Some(cwd) = cwd {
            command.current_dir(cwd);
        }
        let mut child = command.spawn().map_err(|_| EngineError::Unavailable)?;
        let stdout = child.stdout.take().ok_or(EngineError::Unavailable)?;
        let stderr = child.stderr.take().ok_or(EngineError::Unavailable)?;
        let (out_tx, out_rx) = mpsc::channel();
        let (err_tx, err_rx) = mpsc::channel();
        let readers = [
            std::thread::spawn(move || {
                let _ = out_tx.send(read_bounded(stdout, MAX_STDOUT));
            }),
            std::thread::spawn(move || {
                let _ = err_tx.send(read_bounded(stderr, MAX_STDERR));
            }),
        ];
        let until = Instant::now() + self.timeout;
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break Some(status),
                Ok(None) if Instant::now() >= until => {
                    // restic is Vesta's own helper; stopping it never touches the game.
                    let _ = child.kill();
                    let _ = child.wait();
                    break None;
                }
                Ok(None) => std::thread::sleep(POLL),
                Err(_) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(EngineError::Failed);
                }
            }
        };
        for reader in readers {
            let _ = reader.join();
        }
        let stdout = out_rx.recv().unwrap_or(None);
        let _stderr = err_rx.recv().unwrap_or(None);
        let Some(status) = status else {
            return Err(EngineError::TimedOut);
        };
        let stdout = stdout.ok_or(EngineError::Protocol)?;
        match status.code() {
            Some(0) => Ok(Output { stdout }),
            Some(3) => Err(EngineError::Incomplete),
            Some(11) => Err(EngineError::Locked),
            _ => Err(EngineError::Failed),
        }
    }

    /// Create the repository (encrypted with the password file).
    pub fn init(&self) -> Result<()> {
        self.run(&["init".as_ref()], None).map(|_| ())
    }

    /// Back up the current directory tree of `root` (paths stored relative to it).
    pub fn backup(
        &self,
        root: &Path,
        exclude_file: Option<&Path>,
        tags: &[String],
    ) -> Result<BackupSummary> {
        let mut args: Vec<std::ffi::OsString> = vec!["backup".into(), ".".into()];
        if let Some(exclude) = exclude_file {
            args.push("--exclude-file".into());
            args.push(exclude.as_os_str().to_owned());
        }
        for tag in tags {
            if !valid_tag(tag) {
                return Err(EngineError::Invalid);
            }
            args.push("--tag".into());
            args.push(tag.into());
        }
        let refs: Vec<&std::ffi::OsStr> = args.iter().map(|a| a.as_os_str()).collect();
        let output = self.run(&refs, Some(root))?;
        parse_summary(&output.stdout)
    }

    pub fn snapshots(&self, tag: &str) -> Result<Vec<Snapshot>> {
        if !valid_tag(tag) {
            return Err(EngineError::Invalid);
        }
        let output = self.run(
            &["snapshots".as_ref(), "--tag".as_ref(), tag.as_ref()],
            None,
        )?;
        parse_snapshots(&output.stdout)
    }

    /// Add tags to one snapshot (used to promote a checked candidate into its profile).
    pub fn tag_add(&self, snapshot: &str, tags: &[String]) -> Result<()> {
        if !valid_snapshot_id(snapshot) || tags.is_empty() || tags.iter().any(|t| !valid_tag(t)) {
            return Err(EngineError::Invalid);
        }
        let mut args: Vec<std::ffi::OsString> = vec!["tag".into()];
        for tag in tags {
            args.push("--add".into());
            args.push(tag.into());
        }
        args.push(snapshot.into());
        let refs: Vec<&std::ffi::OsStr> = args.iter().map(|a| a.as_os_str()).collect();
        self.run(&refs, None).map(|_| ())
    }

    /// Apply retention to one profile's snapshots and prune unreferenced data.
    pub fn forget(&self, tag: &str, retention: Retention) -> Result<()> {
        if !valid_tag(tag) || retention.is_empty() {
            return Err(EngineError::Invalid);
        }
        let mut args: Vec<std::ffi::OsString> = vec![
            "forget".into(),
            "--prune".into(),
            "--tag".into(),
            tag.into(),
        ];
        for (flag, value) in [
            ("--keep-last", retention.keep_last),
            ("--keep-daily", retention.keep_daily),
            ("--keep-weekly", retention.keep_weekly),
            ("--keep-monthly", retention.keep_monthly),
        ] {
            if value > 0 {
                args.push(flag.into());
                args.push(value.to_string().into());
            }
        }
        let refs: Vec<&std::ffi::OsStr> = args.iter().map(|a| a.as_os_str()).collect();
        self.run(&refs, None).map(|_| ())
    }

    /// Verify repository structure, optionally reading a percentage of pack data.
    pub fn check(&self, read_data_percent: Option<u8>) -> Result<()> {
        let mut args: Vec<std::ffi::OsString> = vec!["check".into()];
        if let Some(percent) = read_data_percent {
            if !(1..=100).contains(&percent) {
                return Err(EngineError::Invalid);
            }
            args.push(format!("--read-data-subset={percent}%").into());
        }
        let refs: Vec<&std::ffi::OsStr> = args.iter().map(|a| a.as_os_str()).collect();
        self.run(&refs, None).map(|_| ())
    }

    /// Restore a snapshot into an empty target directory, optionally limited to
    /// validated relative paths.
    pub fn restore(&self, snapshot: &str, target: &Path, includes: &[String]) -> Result<()> {
        if !valid_snapshot_id(snapshot) {
            return Err(EngineError::Invalid);
        }
        let mut args: Vec<std::ffi::OsString> = vec![
            "restore".into(),
            snapshot.into(),
            "--target".into(),
            target.as_os_str().to_owned(),
        ];
        for include in includes {
            args.push("--include".into());
            args.push(format!("/{include}").into());
        }
        let refs: Vec<&std::ffi::OsStr> = args.iter().map(|a| a.as_os_str()).collect();
        self.run(&refs, None).map(|_| ())
    }
}

fn read_bounded(mut reader: impl Read, maximum: usize) -> Option<Vec<u8>> {
    let mut bytes = Vec::new();
    let mut chunk = [0; 8192];
    let mut overflow = false;
    loop {
        match reader.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                if bytes.len() + n > maximum {
                    overflow = true;
                } else {
                    bytes.extend_from_slice(&chunk[..n]);
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => return None,
        }
    }
    (!overflow).then_some(bytes)
}

#[derive(Deserialize)]
struct WireSummary {
    message_type: String,
    #[serde(default)]
    snapshot_id: Option<String>,
    #[serde(default)]
    files_new: u64,
    #[serde(default)]
    files_changed: u64,
    #[serde(default)]
    files_unmodified: u64,
    #[serde(default)]
    data_added: u64,
    #[serde(default)]
    total_bytes_processed: u64,
}

/// `restic backup --json` emits one JSON object per line; the last `summary` wins.
fn parse_summary(stdout: &[u8]) -> Result<BackupSummary> {
    let text = std::str::from_utf8(stdout).map_err(|_| EngineError::Protocol)?;
    let mut summary = None;
    for line in text.lines().filter(|line| line.starts_with('{')) {
        let Ok(message) = serde_json::from_str::<WireSummary>(line) else {
            continue;
        };
        if message.message_type == "summary" {
            let id = message.snapshot_id.ok_or(EngineError::Protocol)?;
            if !valid_snapshot_id(&id) {
                return Err(EngineError::Protocol);
            }
            summary = Some(BackupSummary {
                snapshot_id: id,
                files_new: message.files_new,
                files_changed: message.files_changed,
                files_unmodified: message.files_unmodified,
                data_added: message.data_added,
                total_bytes_processed: message.total_bytes_processed,
            });
        }
    }
    summary.ok_or(EngineError::Protocol)
}

#[derive(Deserialize)]
struct WireSnapshot {
    id: String,
    time: String,
    #[serde(default)]
    tags: Vec<String>,
}

fn parse_snapshots(stdout: &[u8]) -> Result<Vec<Snapshot>> {
    let snapshots: Vec<WireSnapshot> =
        serde_json::from_slice(stdout).map_err(|_| EngineError::Protocol)?;
    if snapshots.len() > MAX_SNAPSHOTS {
        return Err(EngineError::Protocol);
    }
    snapshots
        .into_iter()
        .map(|s| {
            if !valid_snapshot_id(&s.id)
                || !valid_time(&s.time)
                || s.tags.len() > 16
                || s.tags.iter().any(|t| !valid_tag(t))
            {
                return Err(EngineError::Protocol);
            }
            Ok(Snapshot {
                id: s.id,
                time: s.time,
                tags: s.tags,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summaries_snapshots_and_identifiers_are_strict() {
        let out = br#"{"message_type":"status","percent_done":0.5}
{"message_type":"summary","snapshot_id":"0123456789abcdef","files_new":3,"files_changed":1,"files_unmodified":9,"data_added":42,"total_bytes_processed":100}
"#;
        let summary = parse_summary(out).unwrap();
        assert_eq!(summary.snapshot_id, "0123456789abcdef");
        assert_eq!(summary.files_new, 3);
        assert_eq!(
            parse_summary(br#"{"message_type":"summary","snapshot_id":"../x"}"#),
            Err(EngineError::Protocol)
        );
        assert_eq!(parse_summary(b"not json"), Err(EngineError::Protocol));
        let snaps = parse_snapshots(
            br#"[{"id":"0123456789abcdef0123","time":"2026-10-12T01:02:03Z","tags":["vesta","profile=nightly"],"paths":["/x"]}]"#,
        )
        .unwrap();
        assert_eq!(snaps[0].tags, ["vesta", "profile=nightly"]);
        assert!(parse_snapshots(br#"[{"id":"zz","time":"t"}]"#).is_err());
        assert!(
            parse_snapshots(
                br#"[{"id":"0123456789abcdef","time":"<script>alert(1)</script>xxxxx"}]"#
            )
            .is_err()
        );
        assert!(valid_time("2026-10-12T01:02:03.123456789+02:00"));
        assert!(valid_snapshot_id("abcdef01"));
        assert!(!valid_snapshot_id("ABCDEF01"));
        assert!(!valid_snapshot_id("abc"));
        assert!(!valid_tag("profile=a b"));
    }

    #[test]
    fn a_missing_executable_is_unavailable_not_a_panic() {
        let engine = ResticEngine::new(
            PathBuf::from("Z:/definitely/not/restic.exe"),
            PathBuf::from("Z:/repo"),
            PathBuf::from("Z:/key"),
            PathBuf::from("Z:/cache"),
            Duration::from_secs(5),
        );
        assert_eq!(engine.init(), Err(EngineError::Unavailable));
        assert_eq!(
            engine.forget("vesta", Retention::default()),
            Err(EngineError::Invalid)
        );
        assert_eq!(
            engine.restore("not-hex", Path::new("Z:/t"), &[]),
            Err(EngineError::Invalid)
        );
    }
}
