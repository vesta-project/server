package com.vesta.wrapper.state;

import org.sqlite.SQLiteConfig;

import java.nio.file.Files;
import java.nio.file.LinkOption;
import java.nio.file.Path;
import java.sql.Connection;
import java.sql.DriverManager;
import java.sql.ResultSet;
import java.sql.SQLException;
import java.sql.Statement;
import java.time.Clock;
import java.util.List;
import java.util.Objects;
import java.util.concurrent.locks.ReentrantLock;
import java.util.concurrent.locks.ReentrantReadWriteLock;

/**
 * Local SQLite (WAL) state store with versioned migrations.
 *
 * <p>Every connection enforces foreign keys and a busy timeout and is closed after use.
 * Writes run in {@code BEGIN IMMEDIATE} transactions that roll back on any failure and are
 * serialized in-process. Opening fails closed on integrity/foreign-key check failures,
 * on a newer-than-supported schema, or if the pre-migration backup cannot be made and verified.
 */
public final class StateDatabase implements AutoCloseable {
    /** 1 = foundation, 2 = persisted operation inputs/owner tokens, 3 = identity (IdentitySchema). */
    public static final int CURRENT_SCHEMA_VERSION = 3;
    static final int BUSY_TIMEOUT_MS = 5_000;

    /** Unit of work against a connection owned by the database. */
    @FunctionalInterface
    public interface SqlWork<T> {
        T run(Connection connection) throws SQLException;
    }

    private final StateDirectory directory;
    private final Clock clock;
    private final String url;
    private final ReentrantLock writeLock = new ReentrantLock(true);
    /**
     * Lifecycle guard: every read/write holds the shared side for its whole duration; close takes the
     * exclusive side, so it waits for in-flight calls and all later calls are rejected. Fair, so a
     * pending close is not starved by a stream of new calls (re-entrant nested reads still proceed).
     */
    private final ReentrantReadWriteLock lifecycle = new ReentrantReadWriteLock(true);
    private volatile boolean closed;
    private String serverId;
    /**
     * Lifecycle-owned connection held open (idle, autocommit, no transaction) from open until close.
     * While any connection is open SQLite never deletes/recreates the -wal/-shm companions, so
     * short-lived per-call connections closing cannot cause companion-file churn that races the
     * per-connection link/special-file checks. Idle autocommit means it never blocks checkpoints.
     */
    private Connection anchor;

    private StateDatabase(StateDirectory directory, Clock clock) {
        this.directory = directory;
        this.clock = clock;
        this.url = "jdbc:sqlite:" + directory.databaseFile().toAbsolutePath();
    }

    public static StateDatabase open(StateDirectory directory, Clock clock) {
        Objects.requireNonNull(directory, "directory");
        Objects.requireNonNull(clock, "clock");
        directory.prepare();
        StateDatabase db = new StateDatabase(directory, clock);
        db.initialize();
        return db;
    }

    public String serverId() {
        return serverId;
    }

    public Clock clock() {
        return clock;
    }

    public StateDirectory directory() {
        return directory;
    }

    public int schemaVersion() {
        return read(StateDatabase::userVersion);
    }

    /** Runs {@code work} in a write transaction; commits on success, rolls back on any throwable. */
    public <T> T write(SqlWork<T> work) {
        if (writeLock.isHeldByCurrentThread()) {
            throw new StateException("nested_transaction", "Nested write transactions are not supported");
        }
        enter();
        try {
            writeLock.lock();
            try (Connection c = connect(SQLiteConfig.TransactionMode.IMMEDIATE, false)) {
                c.setAutoCommit(false);
                try {
                    T result = work.run(c);
                    c.commit();
                    return result;
                } catch (Throwable t) {
                    rollbackQuietly(c, t);
                    throw wrap(t);
                }
            } catch (SQLException e) {
                throw new StateException("state_io", "State database write failed", e);
            } finally {
                writeLock.unlock();
            }
        } finally {
            lifecycle.readLock().unlock();
        }
    }

    /** Runs {@code work} in a read-only snapshot transaction. */
    public <T> T read(SqlWork<T> work) {
        enter();
        try (Connection c = connect(SQLiteConfig.TransactionMode.DEFERRED, true)) {
            c.setAutoCommit(false);
            try {
                return work.run(c);
            } catch (Throwable t) {
                throw wrap(t);
            } finally {
                rollbackQuietly(c, null);
            }
        } catch (SQLException e) {
            throw new StateException("state_io", "State database read failed", e);
        } finally {
            lifecycle.readLock().unlock();
        }
    }

    /**
     * Rejects new calls, waits for in-flight reads/writes to finish, then closes the anchor connection
     * (letting SQLite checkpoint and clean up its companion files). Idempotent. Must not be called from
     * inside a read/write callback.
     */
    @Override
    public void close() {
        if (lifecycle.getReadHoldCount() > 0) {
            throw new StateException("close_in_callback", "Cannot close the state database from inside a callback");
        }
        lifecycle.writeLock().lock();
        try {
            if (closed) {
                return;
            }
            closed = true;
            closeAnchor();
        } finally {
            lifecycle.writeLock().unlock();
        }
    }

    /** Acquires the shared lifecycle guard; caller must release it. Fails if closed. */
    private void enter() {
        lifecycle.readLock().lock();
        if (closed) {
            lifecycle.readLock().unlock();
            throw new StateException("state_closed", "State database is closed");
        }
    }

    private void closeAnchor() {
        Connection a = anchor;
        anchor = null;
        if (a != null) {
            try {
                a.close();
            } catch (SQLException e) {
                throw new StateException("state_close_failed", "State database did not close cleanly", e);
            }
        }
    }

    // ---------------------------------------------------------------- internals

    private void initialize() {
        Path dbFile = directory.databaseFile();
        boolean existed = Files.exists(dbFile, LinkOption.NOFOLLOW_LINKS);
        Connection c = null;
        try {
            // Initialization is single-threaded (the instance is not yet published). This connection
            // becomes the lifecycle anchor on success and is closed on any failure.
            c = connect(SQLiteConfig.TransactionMode.IMMEDIATE, false);
            directory.tightenFile(dbFile);
            try (Statement s = c.createStatement(); ResultSet rs = s.executeQuery("PRAGMA journal_mode=WAL")) {
                if (!rs.next() || !"wal".equalsIgnoreCase(rs.getString(1))) {
                    throw new StateException("state_wal_unavailable", "SQLite WAL mode could not be enabled");
                }
            }
            if (existed) {
                checkIntegrity(c);
            }
            int version = userVersion(c);
            if (version > CURRENT_SCHEMA_VERSION) {
                throw new StateException("state_schema_too_new",
                        "State schema version " + version + " is newer than supported " + CURRENT_SCHEMA_VERSION);
            }
            if (version < CURRENT_SCHEMA_VERSION && existed) {
                backupBeforeMigration(c, version);
            }
            for (int v = version + 1; v <= CURRENT_SCHEMA_VERSION; v++) {
                applyMigration(c, v);
            }
            checkIntegrity(c);
            serverId = loadServerId(c);
            if (!c.getAutoCommit()) {
                throw new StateException("state_open_failed", "Anchor connection left inside a transaction");
            }
            // Companions are stable now: the anchor keeps them alive until close.
            for (String suffix : List.of("-wal", "-shm")) {
                directory.tightenFile(dbFile.resolveSibling(dbFile.getFileName() + suffix));
            }
            anchor = c;
        } catch (SQLException e) {
            closeQuietly(c);
            throw new StateException("state_open_failed", "Cannot open state database", e);
        } catch (RuntimeException | Error e) {
            closeQuietly(c);
            throw e;
        }
    }

    private static void closeQuietly(Connection c) {
        if (c != null) {
            try {
                c.close();
            } catch (SQLException ignored) {
                // already failing; original error is reported
            }
        }
    }

    private void applyMigration(Connection c, int version) throws SQLException {
        List<String> statements = Migrations.statements(version, clock.millis());
        c.setAutoCommit(false);
        try (Statement s = c.createStatement()) {
            for (String sql : statements) {
                s.execute(sql);
            }
            s.execute("PRAGMA user_version = " + version);
            c.commit();
        } catch (SQLException | RuntimeException e) {
            rollbackQuietly(c, e);
            throw new StateException("state_migration_failed", "Migration to schema " + version + " failed", e);
        } finally {
            c.setAutoCommit(true);
        }
    }

    private void backupBeforeMigration(Connection c, int fromVersion) throws SQLException {
        Path target = directory.backupsDir().resolve(
                "pre-migration-v" + fromVersion + "-to-v" + CURRENT_SCHEMA_VERSION + "-" + clock.millis() + ".db");
        String file = target.toAbsolutePath().toString();
        directory.verifyDatabaseFiles(); // backups dir must be a real directory, not a link
        if (file.indexOf('"') >= 0 || Files.exists(target, LinkOption.NOFOLLOW_LINKS) || Files.isSymbolicLink(target)) {
            throw new StateException("state_backup_failed", "Unusable pre-migration backup path");
        }
        // sqlite-jdbc "backup to" uses SQLite's online backup API.
        try (Statement s = c.createStatement()) {
            s.executeUpdate("backup to \"" + file + "\"");
        } catch (SQLException e) {
            throw new StateException("state_backup_failed", "Pre-migration backup failed", e);
        }
        StateDirectory.requireRegularOrAbsent(target);
        if (!Files.exists(target, LinkOption.NOFOLLOW_LINKS)) {
            throw new StateException("state_backup_failed", "Pre-migration backup was not created");
        }
        directory.tightenFile(target);
        try (Connection b = DriverManager.getConnection("jdbc:sqlite:" + file)) {
            checkIntegrity(b);
        } catch (SQLException | StateException e) {
            throw new StateException("state_backup_failed", "Pre-migration backup failed verification", e);
        }
    }

    private Connection connect(SQLiteConfig.TransactionMode mode, boolean queryOnly) throws SQLException {
        // Refuse linked/reparse/special database, WAL, SHM or journal files before SQLite touches them.
        directory.verifyDatabaseFiles();
        SQLiteConfig config = new SQLiteConfig();
        config.enforceForeignKeys(true);
        config.setBusyTimeout(BUSY_TIMEOUT_MS);
        config.setSynchronous(SQLiteConfig.SynchronousMode.FULL);
        config.setTransactionMode(mode);
        Connection c = DriverManager.getConnection(url, config.toProperties());
        try (Statement s = c.createStatement()) {
            try (ResultSet rs = s.executeQuery("PRAGMA foreign_keys")) {
                if (!rs.next() || rs.getInt(1) != 1) {
                    throw new StateException("state_foreign_keys_disabled", "Foreign keys could not be enforced");
                }
            }
            if (queryOnly) {
                s.execute("PRAGMA query_only = ON");
            }
        } catch (SQLException | RuntimeException e) {
            c.close();
            throw e;
        }
        return c;
    }

    private static void checkIntegrity(Connection c) throws SQLException {
        try (Statement s = c.createStatement()) {
            try (ResultSet rs = s.executeQuery("PRAGMA integrity_check")) {
                if (!rs.next() || !"ok".equalsIgnoreCase(rs.getString(1))) {
                    throw new StateException("state_integrity_failed", "State database integrity check failed");
                }
            }
            try (ResultSet rs = s.executeQuery("PRAGMA foreign_key_check")) {
                if (rs.next()) {
                    throw new StateException("state_integrity_failed", "State database foreign key check failed");
                }
            }
        }
    }

    private static int userVersion(Connection c) throws SQLException {
        try (Statement s = c.createStatement(); ResultSet rs = s.executeQuery("PRAGMA user_version")) {
            return rs.next() ? rs.getInt(1) : 0;
        }
    }

    private static String loadServerId(Connection c) throws SQLException {
        try (Statement s = c.createStatement();
             ResultSet rs = s.executeQuery("SELECT server_id FROM server_identity WHERE singleton = 1")) {
            if (!rs.next()) {
                throw new StateException("state_identity_missing", "Durable server identity is missing");
            }
            return rs.getString(1);
        }
    }

    private static void rollbackQuietly(Connection c, Throwable primary) {
        try {
            if (!c.getAutoCommit()) {
                c.rollback();
            }
        } catch (SQLException e) {
            if (primary != null) {
                primary.addSuppressed(e);
            }
        }
    }

    private static RuntimeException wrap(Throwable t) {
        if (t instanceof RuntimeException re) {
            return re;
        }
        if (t instanceof Error err) {
            throw err;
        }
        return new StateException("state_io", "State database operation failed", t);
    }
}
