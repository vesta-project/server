use crate::{Clock, Result, StateDirectory, StateError, TransactionError, directory, migrations};
use rusqlite::{
    Connection, DropBehavior, OpenFlags, OptionalExtension, Transaction, TransactionBehavior,
    backup::{Backup, StepResult},
};
use std::{
    cell::RefCell,
    collections::VecDeque,
    sync::{Arc, Condvar, Mutex, MutexGuard},
    time::{Duration, Instant},
};

pub const CURRENT_SCHEMA_VERSION: u32 = 7;
pub const BUSY_TIMEOUT_MS: u64 = 5_000;

/// A callback-borrowed, already-established query-only snapshot.
/// The SQL seam is for trusted repositories, never caller-selected SQL.
///
/// ```compile_fail
/// # use vesta_state::{StateDatabase, StateError};
/// # fn escape(db: &StateDatabase) {
/// let connection = db.read_with(|snapshot| {
///     Ok::<_, StateError>(snapshot.connection())
/// });
/// # }
/// ```
pub struct ReadTransaction<'connection> {
    connection: &'connection Connection,
}

impl std::fmt::Debug for ReadTransaction<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ReadTransaction")
            .finish_non_exhaustive()
    }
}

impl ReadTransaction<'_> {
    pub fn connection(&self) -> &Connection {
        self.connection
    }
}

/// A callback-borrowed immediate transaction. The database owns commit and rollback.
/// Resolve current authority, mutate, and append audit using this one SQL connection.
///
/// ```compile_fail
/// # use vesta_state::{StateDatabase, StateError};
/// # fn escape(db: &StateDatabase) {
/// let connection = db.write_with(|transaction| {
///     Ok::<_, StateError>(transaction.connection())
/// });
/// # }
/// ```
pub struct WriteTransaction<'connection> {
    connection: &'connection Connection,
}

impl std::fmt::Debug for WriteTransaction<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WriteTransaction")
            .finish_non_exhaustive()
    }
}

impl WriteTransaction<'_> {
    /// Trusted repository seam; never execute caller-selected SQL or transaction control.
    pub fn connection(&self) -> &Connection {
        self.connection
    }
}

/// A cloned handle shares lifecycle, identity, anchor, and FIFO writer admission.
/// Per-call connections never escape their callback and are independently configured.
#[derive(Clone)]
pub struct StateDatabase {
    inner: Arc<Shared>,
}

impl std::fmt::Debug for StateDatabase {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("StateDatabase")
            .finish_non_exhaustive()
    }
}

/// Keeps the database open while an owned runtime may still need transactions.
/// This is lifecycle retention, not authority or a substitute for an OS runner lock.
#[must_use = "retain this lease until all owned runtime work has really finished"]
pub struct RuntimeLease {
    inner: Arc<Shared>,
}

impl std::fmt::Debug for RuntimeLease {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RuntimeLease")
            .finish_non_exhaustive()
    }
}

impl Drop for RuntimeLease {
    fn drop(&mut self) {
        let mut lifecycle = self.inner.lock();
        lifecycle.runtime_owners -= 1;
        self.inner.changed.notify_all();
    }
}

struct Shared {
    directory: StateDirectory,
    clock: Arc<dyn Clock>,
    server_id: String,
    lifecycle: Mutex<Lifecycle>,
    changed: Condvar,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Open,
    Closing,
    Closed,
}

struct Lifecycle {
    phase: Phase,
    active: usize,
    runtime_owners: usize,
    next_writer: u64,
    writers: VecDeque<u64>,
    writer_active: bool,
    // Idle and autocommit until close. Keeps WAL/SHM alive across short-lived connections.
    anchor: Option<Connection>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CallKind {
    Read,
    Write,
}

thread_local! {
    // Instance identity, not path: independent database instances have independent gates.
    static CALLS: RefCell<Vec<(usize, CallKind)>> = const { RefCell::new(Vec::new()) };
}

impl StateDatabase {
    pub fn open(directory: StateDirectory, clock: Arc<dyn Clock>) -> Result<Self> {
        directory.prepare()?;
        let existed = directory::require_regular_or_absent(directory.database_file())?;
        // Never let SQLite create a main file with default/unverified permissions.
        let reservation = if existed {
            None
        } else {
            Some(directory.reserve_file(directory.database_file())?)
        };
        let anchor = connect(&directory, false)?;
        check_integrity(&anchor)?;
        let version = user_version(&anchor)?;
        if version > CURRENT_SCHEMA_VERSION {
            return Err(StateError::new("state_schema_too_new"));
        }
        let mode: String = anchor
            .query_row("PRAGMA journal_mode=WAL", [], |row| row.get(0))
            .map_err(|_| StateError::new("state_wal_unavailable"))?;
        if !mode.eq_ignore_ascii_case("wal") {
            return Err(StateError::new("state_wal_unavailable"));
        }
        tighten_companions(&directory)?;
        if existed && version < CURRENT_SCHEMA_VERSION {
            backup_before_migration(&directory, &anchor, version, clock.millis())?;
        }
        for next in version + 1..=CURRENT_SCHEMA_VERSION {
            apply_migration(&anchor, next, clock.millis())?;
            tighten_companions(&directory)?;
        }
        check_integrity(&anchor)?;
        let server_id: Option<String> = anchor
            .query_row(
                "SELECT server_id FROM server_identity WHERE singleton = 1",
                [],
                |row| row.get(0),
            )
            .optional()
            .map_err(|_| StateError::new("state_identity_missing"))?;
        let server_id = server_id.ok_or_else(|| StateError::new("state_identity_missing"))?;
        if uuid::Uuid::parse_str(&server_id).is_err() {
            return Err(StateError::new("state_identity_missing"));
        }
        if !anchor.is_autocommit() {
            return Err(StateError::new("state_open_failed"));
        }
        directory.verify_database_files()?;
        tighten_companions(&directory)?;
        drop(reservation);
        Ok(Self {
            inner: Arc::new(Shared {
                directory,
                clock,
                server_id,
                lifecycle: Mutex::new(Lifecycle {
                    phase: Phase::Open,
                    active: 0,
                    runtime_owners: 0,
                    next_writer: 0,
                    writers: VecDeque::new(),
                    writer_active: false,
                    anchor: Some(anchor),
                }),
                changed: Condvar::new(),
            }),
        })
    }

    pub fn server_id(&self) -> &str {
        &self.inner.server_id
    }

    pub fn clock(&self) -> &dyn Clock {
        self.inner.clock.as_ref()
    }

    pub fn directory(&self) -> &StateDirectory {
        &self.inner.directory
    }

    pub fn schema_version(&self) -> Result<u32> {
        self.read(user_version)
    }

    /// State-only convenience API over the same snapshot implementation as read_with.
    /// Use read_with when the repository has its own error or must retain rollback context.
    pub fn read<T>(&self, work: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        self.read_with(|snapshot| work(snapshot.connection()))
            .map_err(TransactionError::into_state)
    }

    /// A query-only snapshot with a typed repository error; nothing can borrow it after return.
    pub fn read_with<T, E>(
        &self,
        work: impl FnOnce(&ReadTransaction<'_>) -> std::result::Result<T, E>,
    ) -> std::result::Result<T, TransactionError<E>> {
        let _admission = self.enter(CallKind::Read)?;
        let connection = connect(&self.inner.directory, true)?;
        let transaction = Transaction::new_unchecked(&connection, TransactionBehavior::Deferred)
            .map_err(StateError::from)?;
        // BEGIN DEFERRED alone doesn't fix a snapshot until its first database read.
        transaction
            .query_row("SELECT count(*) FROM sqlite_schema", [], |row| {
                row.get::<_, i64>(0)
            })
            .map_err(StateError::from)?;
        let result = match work(&ReadTransaction {
            connection: &transaction,
        }) {
            Ok(result) => result,
            Err(error) => return rollback(transaction, TransactionError::Work(error)),
        };
        if transaction.is_autocommit() {
            return Err(StateError::new("state_transaction_ended").into());
        }
        transaction
            .rollback()
            .map_err(|_| StateError::new("state_rollback_failed"))?;
        Ok(result)
    }

    /// State-only convenience API; a returned Err or unwind never commits callback work.
    pub fn write<T>(&self, work: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        self.write_with(|transaction| work(transaction.connection()))
            .map_err(TransactionError::into_state)
    }

    /// FIFO immediate transaction with typed rejection and retained rollback failure.
    /// On unwinding the RAII transaction rolls back and the per-call connection is disposed.
    pub fn write_with<T, E>(
        &self,
        work: impl FnOnce(&WriteTransaction<'_>) -> std::result::Result<T, E>,
    ) -> std::result::Result<T, TransactionError<E>> {
        let _admission = self.enter(CallKind::Write)?;
        let connection = connect(&self.inner.directory, false)?;
        let mut transaction =
            Transaction::new_unchecked(&connection, TransactionBehavior::Immediate)
                .map_err(StateError::from)?;
        let result = match work(&WriteTransaction {
            connection: &transaction,
        }) {
            Ok(result) => result,
            Err(error) => return rollback(transaction, TransactionError::Work(error)),
        };
        if transaction.is_autocommit() {
            return Err(StateError::new("state_transaction_ended").into());
        }
        // Verify before commit: a path refusal must not follow a durable mutation.
        if let Err(error) = self
            .inner
            .directory
            .verify_database_files()
            .and_then(|()| tighten_companions(&self.inner.directory))
        {
            return rollback(transaction, TransactionError::State(error));
        }
        // A failed COMMIT can leave the transaction active (e.g. a deferred FK violation).
        // Own the subsequent rollback explicitly instead of losing its failure in Drop.
        transaction.set_drop_behavior(DropBehavior::Ignore);
        if transaction.commit().is_err() {
            let primary = TransactionError::State(StateError::new("state_commit_failed"));
            if !connection.is_autocommit() && connection.execute_batch("ROLLBACK").is_err() {
                return Err(TransactionError::Rollback {
                    primary: Box::new(primary),
                    failure: StateError::new("state_rollback_failed"),
                });
            }
            return Err(primary);
        }
        Ok(result)
    }

    /// Retain lifecycle ownership independently of any one transaction callback.
    /// A busy close leaves admissions open so the owner can finish and record its outcome.
    pub fn retain_runtime(&self) -> Result<RuntimeLease> {
        if self.current_call().is_some() {
            return Err(StateError::new("state_nested_call"));
        }
        let mut lifecycle = self.inner.lock();
        if lifecycle.phase != Phase::Open {
            return Err(StateError::new("state_closed"));
        }
        lifecycle.runtime_owners = lifecycle
            .runtime_owners
            .checked_add(1)
            .ok_or_else(|| StateError::new("state_io"))?;
        Ok(RuntimeLease {
            inner: self.inner.clone(),
        })
    }

    /// Refuse closure while runtime leases exist, without rejecting their transactions.
    /// Otherwise reject further admissions, drain admitted reads/queued writers, and close
    /// the anchor. Concurrent closers wait for completion; subsequent close calls are no-ops.
    pub fn close(&self) -> Result<()> {
        if self.current_call().is_some() {
            return Err(StateError::new("state_close_in_callback"));
        }
        let mut lifecycle = self.inner.lock();
        match lifecycle.phase {
            Phase::Closed => return Ok(()),
            Phase::Closing => {
                while lifecycle.phase != Phase::Closed {
                    lifecycle = self.inner.wait(lifecycle);
                }
                return Ok(());
            }
            Phase::Open => {
                if lifecycle.runtime_owners != 0 {
                    return Err(StateError::new("state_runtime_busy"));
                }
                lifecycle.phase = Phase::Closing;
            }
        }
        self.inner.changed.notify_all();
        while lifecycle.active != 0 {
            lifecycle = self.inner.wait(lifecycle);
        }
        let anchor = lifecycle.anchor.take();
        drop(lifecycle);
        // This completion guard also unblocks concurrent closers if a future close path unwinds.
        let _completion = CloseCompletion {
            shared: &self.inner,
        };
        if let Some(anchor) = anchor {
            if let Err((remaining, _)) = anchor.close() {
                drop(remaining);
                return Err(StateError::new("state_close_failed"));
            }
        }
        Ok(())
    }

    fn key(&self) -> usize {
        Arc::as_ptr(&self.inner) as usize
    }

    fn current_call(&self) -> Option<CallKind> {
        CALLS.with(|calls| {
            calls
                .borrow()
                .iter()
                .find(|(key, _)| *key == self.key())
                .map(|(_, kind)| *kind)
        })
    }

    fn enter(&self, kind: CallKind) -> Result<Admission<'_>> {
        if let Some(outer) = self.current_call() {
            return Err(StateError::new(
                if outer == CallKind::Write && kind == CallKind::Write {
                    "state_nested_transaction"
                } else {
                    "state_nested_call"
                },
            ));
        }
        let mut lifecycle = self.inner.lock();
        if lifecycle.phase != Phase::Open {
            return Err(StateError::new("state_closed"));
        }
        let active = lifecycle
            .active
            .checked_add(1)
            .ok_or_else(|| StateError::new("state_io"))?;
        let ticket = if kind == CallKind::Write {
            let ticket = lifecycle.next_writer;
            let next = ticket
                .checked_add(1)
                .ok_or_else(|| StateError::new("state_io"))?;
            lifecycle.writers.push_back(ticket);
            lifecycle.next_writer = next;
            Some(ticket)
        } else {
            None
        };
        lifecycle.active = active;
        // Construct RAII before any wait/callback; no mutex is held while user code runs.
        let mut admission = Admission {
            shared: &self.inner,
            key: self.key(),
            ticket,
            writer_held: false,
            tls_registered: false,
        };
        if let Some(ticket) = ticket {
            while lifecycle.writer_active || lifecycle.writers.front().copied() != Some(ticket) {
                lifecycle = self.inner.wait(lifecycle);
            }
            lifecycle.writer_active = true;
            admission.writer_held = true;
        }
        drop(lifecycle);
        CALLS.with(|calls| calls.borrow_mut().push((self.key(), kind)));
        admission.tls_registered = true;
        Ok(admission)
    }
}

impl Shared {
    // No callbacks run under this mutex. Recover poison to allow draining even after an unwind.
    fn lock(&self) -> MutexGuard<'_, Lifecycle> {
        self.lifecycle
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn wait<'a>(&self, guard: MutexGuard<'a, Lifecycle>) -> MutexGuard<'a, Lifecycle> {
        self.changed
            .wait(guard)
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

struct Admission<'a> {
    shared: &'a Shared,
    key: usize,
    ticket: Option<u64>,
    writer_held: bool,
    tls_registered: bool,
}

impl Drop for Admission<'_> {
    fn drop(&mut self) {
        if self.tls_registered {
            CALLS.with(|calls| {
                let mut calls = calls.borrow_mut();
                if let Some(index) = calls.iter().rposition(|(key, _)| *key == self.key) {
                    calls.remove(index);
                }
            });
        }
        let mut lifecycle = self.shared.lock();
        if let Some(ticket) = self.ticket {
            if let Some(index) = lifecycle
                .writers
                .iter()
                .position(|queued| *queued == ticket)
            {
                lifecycle.writers.remove(index);
            }
            if self.writer_held {
                lifecycle.writer_active = false;
            }
        }
        lifecycle.active -= 1;
        self.shared.changed.notify_all();
    }
}

struct CloseCompletion<'a> {
    shared: &'a Shared,
}

impl Drop for CloseCompletion<'_> {
    fn drop(&mut self) {
        self.shared.lock().phase = Phase::Closed;
        self.shared.changed.notify_all();
    }
}

fn rollback<T, E>(
    transaction: Transaction<'_>,
    primary: TransactionError<E>,
) -> std::result::Result<T, TransactionError<E>> {
    match transaction.rollback() {
        Ok(()) => Err(primary),
        Err(_) => Err(TransactionError::Rollback {
            primary: Box::new(primary),
            failure: StateError::new("state_rollback_failed"),
        }),
    }
}

fn connect(directory: &StateDirectory, query_only: bool) -> Result<Connection> {
    directory.verify_database_files()?;
    let flags = OpenFlags::SQLITE_OPEN_READ_WRITE
        | OpenFlags::SQLITE_OPEN_NO_MUTEX
        | OpenFlags::SQLITE_OPEN_NOFOLLOW;
    let connection = Connection::open_with_flags(directory.database_file(), flags)?;
    configure(&connection, query_only)?;
    directory.verify_database_files()?;
    Ok(connection)
}

fn configure(connection: &Connection, query_only: bool) -> Result<()> {
    connection.busy_timeout(Duration::from_millis(BUSY_TIMEOUT_MS))?;
    connection.pragma_update(None, "foreign_keys", "ON")?;
    connection.pragma_update(None, "synchronous", "FULL")?;
    let foreign_keys: i64 =
        connection.pragma_query_value(None, "foreign_keys", |row| row.get(0))?;
    if foreign_keys != 1 {
        return Err(StateError::new("state_foreign_keys_disabled"));
    }
    let synchronous: i64 = connection.pragma_query_value(None, "synchronous", |row| row.get(0))?;
    if synchronous != 2 {
        return Err(StateError::new("state_io"));
    }
    if query_only {
        connection.pragma_update(None, "query_only", "ON")?;
        let value: i64 = connection.pragma_query_value(None, "query_only", |row| row.get(0))?;
        if value != 1 {
            return Err(StateError::new("state_io"));
        }
    }
    Ok(())
}

fn user_version(connection: &Connection) -> Result<u32> {
    let version: i64 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
    u32::try_from(version).map_err(|_| StateError::new("state_schema_invalid"))
}

fn check_integrity(connection: &Connection) -> Result<()> {
    let failure = || StateError::new("state_integrity_failed");
    let mut statement = connection
        .prepare("PRAGMA integrity_check")
        .map_err(|_| failure())?;
    let mut rows = statement.query([]).map_err(|_| failure())?;
    let Some(first) = rows.next().map_err(|_| failure())? else {
        return Err(failure());
    };
    let status: String = first.get(0).map_err(|_| failure())?;
    if !status.eq_ignore_ascii_case("ok") || rows.next().map_err(|_| failure())?.is_some() {
        return Err(failure());
    }
    drop(rows);
    drop(statement);
    let mut statement = connection
        .prepare("PRAGMA foreign_key_check")
        .map_err(|_| failure())?;
    if statement
        .query([])
        .map_err(|_| failure())?
        .next()
        .map_err(|_| failure())?
        .is_some()
    {
        return Err(failure());
    }
    Ok(())
}

fn apply_migration(connection: &Connection, version: u32, now: i64) -> Result<()> {
    let failure = || StateError::new("state_migration_failed");
    let transaction = Transaction::new_unchecked(connection, TransactionBehavior::Immediate)
        .map_err(|_| failure())?;
    // Shared production writers are unsupported; never silently apply a migration to a changed schema.
    if user_version(&transaction).map_err(|_| failure())? != version - 1 {
        return Err(failure());
    }
    migrations::apply(&transaction, version, now).map_err(|_| failure())?;
    transaction
        .pragma_update(None, "user_version", version)
        .map_err(|_| failure())?;
    transaction.commit().map_err(|_| failure())?;
    Ok(())
}

fn tighten_companions(directory: &StateDirectory) -> Result<()> {
    for path in directory::database_paths(directory.database_file()) {
        directory.tighten_file(&path)?;
    }
    Ok(())
}

fn backup_before_migration(
    directory: &StateDirectory,
    source: &Connection,
    version: u32,
    now: i64,
) -> Result<()> {
    let failure = || StateError::new("state_backup_failed");
    let target = directory.backups_dir().join(format!(
        "pre-migration-v{version}-to-v{CURRENT_SCHEMA_VERSION}-{now}.db"
    ));
    directory.verify_database_files()?;
    let reserved = directory.reserve_file(&target).map_err(|_| failure())?;
    // Keep the reservation open: no replacement, truncation, filesystem copies, or raw SQLite FFI.
    let flags = OpenFlags::SQLITE_OPEN_READ_WRITE
        | OpenFlags::SQLITE_OPEN_NO_MUTEX
        | OpenFlags::SQLITE_OPEN_NOFOLLOW;
    let mut destination = Connection::open_with_flags(&target, flags).map_err(|_| failure())?;
    configure(&destination, false).map_err(|_| failure())?;
    {
        let backup = Backup::new(source, &mut destination).map_err(|_| failure())?;
        let started = Instant::now();
        loop {
            // A single whole-snapshot step does not mix schemas between per-chunk restarts.
            match backup.step(-1).map_err(|_| failure())? {
                StepResult::Done => break,
                StepResult::Busy | StepResult::Locked
                    if started.elapsed() < Duration::from_millis(BUSY_TIMEOUT_MS) =>
                {
                    std::thread::sleep(Duration::from_millis(10));
                }
                _ => return Err(failure()),
            }
        }
    }
    // SQLite's backup may copy the source's WAL header. Finish in DELETE mode so the verified
    // artifact is a standalone database, not a main file requiring companion data.
    let mode: String = destination
        .query_row("PRAGMA journal_mode=DELETE", [], |row| row.get(0))
        .map_err(|_| failure())?;
    if !mode.eq_ignore_ascii_case("delete") {
        return Err(failure());
    }
    if user_version(&destination).map_err(|_| failure())? != version {
        return Err(failure());
    }
    check_integrity(&destination).map_err(|_| failure())?;
    if let Err((remaining, _)) = destination.close() {
        drop(remaining);
        return Err(failure());
    }
    for path in directory::database_paths(&target) {
        directory::require_regular_or_absent(&path).map_err(|_| failure())?;
        directory.tighten_file(&path).map_err(|_| failure())?;
    }
    reserved.sync_all().map_err(|_| failure())?;
    directory.verify_database_files().map_err(|_| failure())?;
    // Leave even a failed reserved artifact in place rather than risk unlinking a raced replacement.
    Ok(())
}
