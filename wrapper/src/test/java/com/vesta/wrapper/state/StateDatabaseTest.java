package com.vesta.wrapper.state;

import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.io.TempDir;

import java.nio.file.Files;
import java.nio.file.LinkOption;
import java.nio.file.Path;
import java.nio.file.attribute.AclEntryType;
import java.nio.file.attribute.AclFileAttributeView;
import java.nio.file.attribute.PosixFileAttributeView;
import java.nio.file.attribute.PosixFilePermissions;
import java.sql.DriverManager;
import java.time.Clock;
import java.time.Instant;
import java.time.ZoneOffset;
import java.util.UUID;

import static org.junit.jupiter.api.Assertions.*;

class StateDatabaseTest {
    @TempDir Path root;
    private static final Clock CLOCK = Clock.fixed(Instant.parse("2026-10-04T12:00:00Z"), ZoneOffset.UTC);

    private StateDirectory directory() { return new StateDirectory(root.resolve("private state"), root.resolve("game")); }

    @Test
    void realWalDatabaseHasStableServerIdentityAndConnectionInvariants() {
        StateDirectory directory = directory();
        String id;
        try (StateDatabase db = StateDatabase.open(directory, CLOCK)) {
            id = db.serverId();
            assertDoesNotThrow(() -> UUID.fromString(id));
            assertEquals(StateDatabase.CURRENT_SCHEMA_VERSION, db.schemaVersion());
            db.read(c -> {
                try (var statement = c.createStatement()) {
                    for (String pragma : new String[]{"foreign_keys", "query_only"}) {
                        try (var result = statement.executeQuery("PRAGMA " + pragma)) {
                            assertTrue(result.next());
                            assertEquals(1, result.getInt(1), pragma);
                        }
                    }
                    try (var result = statement.executeQuery("PRAGMA journal_mode")) {
                        assertTrue(result.next());
                        assertEquals("wal", result.getString(1));
                    }
                    try (var result = statement.executeQuery("PRAGMA busy_timeout")) {
                        assertTrue(result.next());
                        assertTrue(result.getInt(1) > 0);
                    }
                    assertThrows(java.sql.SQLException.class,
                            () -> statement.executeUpdate("INSERT INTO meta VALUES ('read-write', 'no')"));
                }
                return null;
            });
            assertThrows(StateException.class, () -> db.write(c -> {
                try (var statement = c.createStatement()) {
                    statement.executeUpdate("INSERT INTO operation_resources VALUES ('missing', 'world')");
                }
                return null;
            }));
        }
        try (StateDatabase reopened = StateDatabase.open(directory, CLOCK)) {
            assertEquals(id, reopened.serverId());
        }
    }

    @Test
    void runtimeFailuresRollBackAndNestedWritesFailWithoutPartialCommit() {
        try (StateDatabase db = StateDatabase.open(directory(), CLOCK)) {
            assertThrows(IllegalStateException.class, () -> db.write(c -> {
                c.createStatement().executeUpdate("INSERT INTO meta VALUES ('must-rollback', 'x')");
                throw new IllegalStateException("injected failure");
            }));
            assertThrows(StateException.class, () -> db.write(c -> db.write(inner -> null)));
            assertEquals(0, db.read(c -> {
                try (var ps = c.prepareStatement("SELECT COUNT(*) FROM meta WHERE key = 'must-rollback'");
                     var result = ps.executeQuery()) {
                    result.next();
                    return result.getInt(1);
                }
            }).intValue());
            db.close();
            assertEquals("state_closed", assertThrows(StateException.class, () -> db.read(c -> null)).getCode());
            assertThrows(StateException.class, () -> db.write(c -> null));
        }
    }

    @Test
    void preMigrationBackupUsesSqliteSnapshotAndPreservesOriginalSchema() throws Exception {
        StateDirectory directory = directory();
        directory.prepare();
        try (var connection = DriverManager.getConnection("jdbc:sqlite:" + directory.databaseFile());
             var statement = connection.createStatement()) {
            statement.execute("PRAGMA journal_mode=WAL");
            statement.execute("CREATE TABLE original (value TEXT)");
            statement.execute("INSERT INTO original VALUES ('before migration')");
            try (StateDatabase migrated = StateDatabase.open(directory, CLOCK)) {
                assertEquals(StateDatabase.CURRENT_SCHEMA_VERSION, migrated.schemaVersion());
            }
        }
        try (var paths = Files.list(directory.backupsDir())) {
            var backups = paths.toList();
            assertEquals(1, backups.size());
            try (var backup = DriverManager.getConnection("jdbc:sqlite:" + backups.get(0));
                 var statement = backup.createStatement()) {
                try (var version = statement.executeQuery("PRAGMA user_version")) {
                    assertTrue(version.next());
                    assertEquals(0, version.getInt(1));
                }
                try (var data = statement.executeQuery("SELECT value FROM original")) {
                    assertTrue(data.next());
                    assertEquals("before migration", data.getString(1));
                }
                try (var integrity = statement.executeQuery("PRAGMA integrity_check")) {
                    assertTrue(integrity.next());
                    assertEquals("ok", integrity.getString(1));
                }
            }
        }
    }

    @Test
    void corruptOrUnsupportedDatabaseFailsClosed() throws Exception {
        StateDirectory corrupt = directory();
        corrupt.prepare();
        Files.writeString(corrupt.databaseFile(), "this is not SQLite");
        assertThrows(StateException.class, () -> StateDatabase.open(corrupt, CLOCK));
        StateDirectory newer = new StateDirectory(root.resolve("newer"));
        newer.prepare();
        try (var connection = DriverManager.getConnection("jdbc:sqlite:" + newer.databaseFile());
             var statement = connection.createStatement()) {
            statement.execute("PRAGMA user_version = 999");
        }
        assertEquals("state_schema_too_new",
                assertThrows(StateException.class, () -> StateDatabase.open(newer, CLOCK)).getCode());
    }

    @Test
    void databaseWithForeignKeyViolationIsRejectedOnReopen() throws Exception {
        StateDirectory directory = directory();
        try (StateDatabase ignored = StateDatabase.open(directory, CLOCK)) { }
        try (var connection = DriverManager.getConnection("jdbc:sqlite:" + directory.databaseFile());
             var statement = connection.createStatement()) {
            statement.executeUpdate("INSERT INTO operation_resources VALUES ('missing', 'world')");
        }
        assertEquals("state_integrity_failed",
                assertThrows(StateException.class, () -> StateDatabase.open(directory, CLOCK)).getCode());
    }

    @Test
    void linkedBackupsDirectoryIsRejectedWithoutTouchingExternalFiles() throws Exception {
        StateDirectory directory = directory();
        Files.createDirectories(directory.path());
        Path outside = Files.createDirectories(root.resolve("external"));
        Files.writeString(outside.resolve("sentinel"), "unchanged");
        com.vesta.wrapper.testutil.NativeTestLinks.directory(directory.backupsDir(), outside);
        try {
            assertEquals("state_file_linked", assertThrows(StateException.class,
                    () -> StateDatabase.open(directory, CLOCK)).getCode());
            assertEquals("unchanged", Files.readString(outside.resolve("sentinel")));
            try (var files = Files.list(outside)) { assertEquals(1, files.count()); }
            assertFalse(Files.exists(directory.databaseFile()));
        } finally { Files.deleteIfExists(directory.backupsDir()); }
    }

    @Test
    void existingConnectionsAlsoRejectAReplacedBackupDirectory() throws Exception {
        StateDirectory directory = directory();
        try (StateDatabase db = StateDatabase.open(directory, CLOCK)) {
            Files.delete(directory.backupsDir());
            Path outside = Files.createDirectories(root.resolve("external"));
            com.vesta.wrapper.testutil.NativeTestLinks.directory(directory.backupsDir(), outside);
            try {
                assertEquals("state_file_linked", assertThrows(StateException.class, () -> db.read(c -> null)).getCode());
                assertThrows(StateException.class, () -> db.write(c -> null));
            } finally { Files.deleteIfExists(directory.backupsDir()); Files.createDirectory(directory.backupsDir()); }
        }
    }

    @Test
    void stateCannotOverlapGameAndUsesNativeOwnerOnlyPermissions() throws Exception {
        Path game = Files.createDirectories(root.resolve("game"));
        assertThrows(StateException.class, () -> new StateDirectory(game.resolve("state"), game));
        assertThrows(StateException.class, () -> new StateDirectory(root, game));
        assertThrows(StateException.class, () -> new StateDirectory(game, game));
        StateDirectory directory = directory();
        try (StateDatabase ignored = StateDatabase.open(directory, CLOCK)) { }
        for (Path path : new Path[]{directory.path(), directory.backupsDir(), directory.databaseFile()}) {
            var posix = Files.getFileAttributeView(path, PosixFileAttributeView.class, LinkOption.NOFOLLOW_LINKS);
            if (posix != null) {
                assertEquals(PosixFilePermissions.fromString(Files.isDirectory(path) ? "rwx------" : "rw-------"),
                        posix.readAttributes().permissions());
            } else {
                var acl = Files.getFileAttributeView(path, AclFileAttributeView.class, LinkOption.NOFOLLOW_LINKS);
                assertNotNull(acl);
                assertFalse(acl.getAcl().isEmpty());
                var owner = acl.getOwner();
                assertTrue(acl.getAcl().stream().allMatch(entry -> entry.type() == AclEntryType.ALLOW
                        && entry.principal().equals(owner)));
            }
        }
    }
}
