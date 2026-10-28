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

/// V27 protected pack draft/upload tables.
pub const REMOVE_V27_SCHEMA: &str = "DROP TABLE pack_publication_history; DROP TABLE pack_workspace_uploads; DROP TABLE pack_workspace_published; DROP TABLE pack_workspace_receipts; DROP TABLE pack_workspace_blobs; DROP TABLE pack_workspace;";

/// V25 public invitation IDs and later workspace tables. Older simulated schemas
/// must remove all subsequent additions, rather than only lower user_version.
#[allow(dead_code)]
pub const REMOVE_V25_SCHEMA: &str = "DROP TABLE pack_publication_history; DROP TABLE pack_workspace_uploads; DROP TABLE pack_workspace_published; DROP TABLE pack_workspace_receipts; DROP TABLE pack_workspace_blobs; DROP TABLE pack_workspace; DROP TABLE management_invitation_ids;";

/// V24 metrics additions. Usable inside an existing transaction.
#[allow(dead_code)]
pub const REMOVE_V24_SCHEMA: &str = "DROP TABLE metrics_policy; ALTER TABLE metrics_status DROP COLUMN prune_revision; ALTER TABLE metrics_status DROP COLUMN revision; ALTER TABLE metrics_daily DROP COLUMN revision; ALTER TABLE metrics_5m DROP COLUMN revision; ALTER TABLE metrics_raw DROP COLUMN revision; ALTER TABLE metrics_raw DROP COLUMN tick_source;";

/// V23 metrics additions. Usable inside an existing transaction.
#[allow(dead_code)]
pub const REMOVE_V23_SCHEMA: &str = "DROP TABLE metrics_activity; DROP TABLE metrics_status; DROP TABLE metrics_daily; DROP TABLE metrics_5m; DROP TABLE metrics_raw;";

/// V22 additions (player moderation). Usable inside an existing transaction.
#[allow(dead_code)]
pub const REMOVE_V22_SCHEMA: &str = "DROP TABLE moderation_actions; DROP TRIGGER role_capabilities_builtin_no_delete; DELETE FROM role_capabilities WHERE capability='players.operator'; CREATE TRIGGER role_capabilities_builtin_no_delete BEFORE DELETE ON role_capabilities WHEN (SELECT builtin FROM roles WHERE id=OLD.role_id)=1 BEGIN SELECT RAISE(ABORT,'builtin roles are immutable'); END;";

/// V21 additions (runtime backup policy). Usable inside an existing transaction.
#[allow(dead_code)]
pub const REMOVE_V21_SCHEMA: &str = "DROP TABLE backup_policy; DROP TRIGGER role_capabilities_builtin_no_delete; DELETE FROM role_capabilities WHERE capability='backups.configure'; CREATE TRIGGER role_capabilities_builtin_no_delete BEFORE DELETE ON role_capabilities WHEN (SELECT builtin FROM roles WHERE id=OLD.role_id)=1 BEGIN SELECT RAISE(ABORT,'builtin roles are immutable'); END;";

/// Remove V18 additions and later automation/backup additions.
pub const REMOVE_V18_SCHEMA: &str = "DROP TABLE backup_policy; DROP TABLE backup_jobs; DROP TABLE notification_state; DROP TABLE notifications; DROP TABLE workflow_schedule; DROP TABLE run_steps; DROP TABLE workflow_runs; DROP TABLE workflows; DROP TRIGGER role_capabilities_builtin_no_delete; DELETE FROM role_capabilities WHERE capability IN ('automation.view','automation.manage','notifications.view','backups.configure'); CREATE TRIGGER role_capabilities_builtin_no_delete BEFORE DELETE ON role_capabilities WHEN (SELECT builtin FROM roles WHERE id=OLD.role_id)=1 BEGIN SELECT RAISE(ABORT,'builtin roles are immutable'); END;";

/// V16 additions (remote permissions). Usable inside an existing transaction.
#[allow(dead_code)]
pub const REMOVE_V16_SCHEMA: &str = "DROP TABLE management_invitation_issuers; DROP TABLE management_permission_receipts; DROP TABLE user_minecraft_names;";

/// Remove current file/pack-management and unified-auth additions before constructing an owned
/// pre-V12 migration fixture. Marker-only downgrades are not old schemas.
pub fn remove_v12_schema(connection: &rusqlite::Connection) {
    connection.execute_batch(REMOVE_V25_SCHEMA).unwrap();
    connection.execute_batch(REMOVE_V24_SCHEMA).unwrap();
    connection.execute_batch(REMOVE_V23_SCHEMA).unwrap();
    connection.execute_batch(REMOVE_V22_SCHEMA).unwrap();
    connection.execute_batch(REMOVE_V18_SCHEMA).unwrap();
    connection.execute_batch("BEGIN IMMEDIATE;
        DROP TABLE management_invitation_issuers;
        DROP TABLE management_permission_receipts;
        DROP TABLE user_minecraft_names;
        DROP TABLE management_file_outcomes;
        DROP TABLE management_pack_outcomes;
        DROP TABLE pack_minimum_policy;
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
