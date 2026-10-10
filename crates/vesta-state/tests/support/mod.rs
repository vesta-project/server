// Each integration-test target uses a different subset of these shared fixture helpers.
#![allow(dead_code)]

use std::io;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use tempfile::TempDir;
use vesta_state::{Clock, StateDatabase, StateDirectory};

pub const DEFAULT_TEST_TIME_MS: i64 = 1_700_000_000_000;

/// Mutable, thread-safe clock for deterministic state and migration tests.
pub struct TestClock {
    millis: AtomicI64,
}

impl TestClock {
    pub fn new(millis: i64) -> Self {
        Self {
            millis: AtomicI64::new(millis),
        }
    }

    pub fn set(&self, millis: i64) {
        self.millis.store(millis, Ordering::Relaxed);
    }

    pub fn advance(&self, delta: i64) -> i64 {
        self.millis.fetch_add(delta, Ordering::Relaxed) + delta
    }
}

impl Clock for TestClock {
    fn millis(&self) -> i64 {
        self.millis.load(Ordering::Relaxed)
    }
}

/// Owns every path and handle used by a database test; the state directory is
/// on the local temporary filesystem and is distinct from the game root.
pub struct Fixture {
    pub root: TempDir,
    pub clock: Arc<TestClock>,
    pub db: StateDatabase,
}

impl Fixture {
    pub fn open() -> Self {
        Self::with_time(DEFAULT_TEST_TIME_MS)
    }

    pub fn with_time(millis: i64) -> Self {
        let root = tempfile::tempdir().expect("create local temporary test directory");
        let state = root.path().join("private state");
        let game = root.path().join("game root");
        let directory = StateDirectory::new(&state, Some(&game))
            .expect("create distinct local state and game paths");
        let clock = Arc::new(TestClock::new(millis));
        let db = StateDatabase::open(directory, clock.clone()).expect("open test state database");
        Self { root, clock, db }
    }

    pub fn state_path(&self) -> std::path::PathBuf {
        self.root.path().join("private state")
    }

    pub fn game_path(&self) -> std::path::PathBuf {
        self.root.path().join("game root")
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        // A close error must not panic during unwinding or prevent TempDir cleanup.
        let _ = self.db.close();
    }
}

/// Create a real directory symlink (or a native Windows junction) within a test root.
pub fn create_directory_link(link: &Path, target: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, link)
    }

    #[cfg(windows)]
    {
        use std::process::{Command, Stdio};
        use std::thread;
        use std::time::{Duration, Instant};

        let mut child = Command::new("cmd.exe")
            .args(["/D", "/C", "mklink", "/J"])
            .arg(link)
            .arg(target)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(status) = child.try_wait()? {
                return if status.success() {
                    Ok(())
                } else {
                    Err(io::Error::other(format!(
                        "cmd.exe mklink /J exited with {status}"
                    )))
                };
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "cmd.exe mklink /J exceeded five seconds",
                ));
            }
            thread::sleep(Duration::from_millis(10));
        }
    }

    #[cfg(not(any(unix, windows)))]
    {
        let _ = (link, target);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "directory links are not implemented on this platform",
        ))
    }
}

/// Remove the link itself without following it into the target directory.
pub fn remove_directory_link(link: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        std::fs::remove_file(link)
    }

    #[cfg(windows)]
    {
        // A junction is represented by a directory entry and must be removed with remove_dir.
        std::fs::remove_dir(link)
    }

    #[cfg(not(any(unix, windows)))]
    {
        std::fs::remove_file(link)
    }
}

/// Remove the current unified-auth additions before constructing an owned
/// pre-V12 migration fixture. Marker-only downgrades are not old schemas.
pub fn remove_v12_schema(connection: &rusqlite::Connection) {
    connection.execute_batch("BEGIN IMMEDIATE;
        DROP TABLE management_device_keys;
        DROP TABLE management_pairing_codes;
        DROP TABLE management_owner_links;
        DROP TABLE user_minecraft_accounts;
        DROP TABLE user_default_roles;
        DROP TABLE device_password_grants;
        DROP TABLE access_role_sources;
        DELETE FROM role_capabilities WHERE role_id IN ('public','player');
        DELETE FROM roles WHERE id IN ('public','player');
        DROP TRIGGER role_capabilities_builtin_no_delete;
        DELETE FROM role_capabilities WHERE capability IN ('public.info','pack.download','pack.realtime');
        CREATE TRIGGER role_capabilities_builtin_no_delete BEFORE DELETE ON role_capabilities
            WHEN (SELECT builtin FROM roles WHERE id=OLD.role_id)=1
            BEGIN SELECT RAISE(ABORT,'builtin roles are immutable'); END;
        COMMIT;").unwrap();
}
