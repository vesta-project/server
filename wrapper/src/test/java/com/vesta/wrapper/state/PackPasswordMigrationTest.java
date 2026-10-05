package com.vesta.wrapper.state;

import com.vesta.wrapper.audit.AuditLog;
import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.io.TempDir;

import java.nio.file.Files;
import java.nio.file.Path;
import java.sql.DriverManager;
import java.time.Clock;
import java.time.Instant;
import java.time.ZoneOffset;

import static org.junit.jupiter.api.Assertions.*;

class PackPasswordMigrationTest {
    @TempDir Path root;
    private static final Clock CLOCK = Clock.fixed(Instant.parse("2026-10-05T12:00:00Z"), ZoneOffset.UTC);

    private StateDirectory directory() { return new StateDirectory(root.resolve("state")); }

    private String oldSchema(StateDirectory directory) throws Exception {
        directory.prepare();
        try (var c = DriverManager.getConnection("jdbc:sqlite:" + directory.databaseFile()); var s = c.createStatement()) {
            s.execute("PRAGMA foreign_keys=ON"); s.execute("PRAGMA journal_mode=WAL");
            for (int version=1; version<=3; version++) for (String sql : Migrations.statements(version,CLOCK.millis())) s.execute(sql);
            s.execute("PRAGMA user_version=3"); s.execute("INSERT INTO meta VALUES ('old-marker','preserved')");
            try (var row=s.executeQuery("SELECT server_id FROM server_identity")) { assertTrue(row.next()); return row.getString(1); }
        }
    }

    @Test void schemaThreeMigrationPreservesIdentityRolesAndOriginalVerifiedBackup() throws Exception {
        StateDirectory directory=directory(); String serverId=oldSchema(directory);
        try (StateDatabase db=StateDatabase.open(directory,CLOCK)) {
            assertEquals(4,db.schemaVersion()); assertEquals(serverId,db.serverId());
            db.read(c -> { try (var s=c.createStatement()) {
                try (var row=s.executeQuery("SELECT value FROM meta WHERE key='old-marker'")) { assertTrue(row.next()); assertEquals("preserved",row.getString(1)); }
                try (var row=s.executeQuery("SELECT count(*) FROM roles WHERE builtin=1")) { assertTrue(row.next()); assertEquals(6,row.getInt(1)); }
                try (var row=s.executeQuery("SELECT count(*) FROM pack_passwords")) { assertTrue(row.next()); assertEquals(0,row.getInt(1)); }
            } return null; });
            assertTrue(new AuditLog(db).verifyChain().valid());
        }
        try (var files=Files.list(directory.backupsDir())) {
            var backups=files.toList(); assertEquals(1,backups.size());
            try (var c=DriverManager.getConnection("jdbc:sqlite:"+backups.get(0)); var s=c.createStatement()) {
                try (var row=s.executeQuery("PRAGMA user_version")) { assertTrue(row.next()); assertEquals(3,row.getInt(1)); }
                try (var row=s.executeQuery("PRAGMA integrity_check")) { assertTrue(row.next()); assertEquals("ok",row.getString(1)); }
                try (var row=s.executeQuery("SELECT server_id FROM server_identity")) { assertTrue(row.next()); assertEquals(serverId,row.getString(1)); }
                try (var row=s.executeQuery("SELECT count(*) FROM sqlite_master WHERE name='pack_passwords'")) { assertTrue(row.next()); assertEquals(0,row.getInt(1)); }
            }
        }
        try (StateDatabase reopened=StateDatabase.open(directory,CLOCK)) { assertEquals(4,reopened.schemaVersion()); assertEquals(serverId,reopened.serverId()); }
        try (var files=Files.list(directory.backupsDir())) { assertEquals(1,files.count()); }
    }

    @Test void freshSchemaHasStrictSingletonForeignKeyAndActiveRevokedConsistencyConstraints() throws Exception {
        StateDirectory directory=directory();
        try (StateDatabase db=StateDatabase.open(directory,CLOCK)) {
            assertEquals(4,db.schemaVersion());
            db.write(c -> { try (var ps=c.prepareStatement("INSERT INTO pack_passwords VALUES (?,1,'bounded-phc',1,1,NULL)")) {
                ps.setString(1,db.serverId()); ps.executeUpdate();
            } return null; });
            for (String invalid : new String[]{
                    "INSERT INTO pack_passwords VALUES ('other-server',1,'x',1,1,NULL)",
                    "INSERT INTO pack_passwords SELECT * FROM pack_passwords",
                    "UPDATE pack_passwords SET revision=0", "UPDATE pack_passwords SET revision=-1",
                    "UPDATE pack_passwords SET phc=NULL", "UPDATE pack_passwords SET phc=''",
                    "UPDATE pack_passwords SET revoked_at=1", "UPDATE pack_passwords SET revision='not-integer'"}) {
                assertThrows(StateException.class, () -> db.write(c -> { try (var s=c.createStatement()) { s.execute(invalid); } return null; }),invalid);
            }
            assertThrows(StateException.class, () -> db.write(c -> { try (var ps=c.prepareStatement("UPDATE pack_passwords SET phc=?")) { ps.setString(1,"x".repeat(257)); ps.executeUpdate(); } return null; }));
            db.write(c -> { try (var s=c.createStatement()) { s.execute("UPDATE pack_passwords SET phc=NULL,revoked_at=2,revision=2"); } return null; });
            assertThrows(StateException.class, () -> db.write(c -> { try (var s=c.createStatement()) { s.execute("UPDATE pack_passwords SET phc='x'"); } return null; }));
        }
        try (var files=Files.list(directory.backupsDir())) { assertEquals(0,files.count()); }
    }

    @Test void failedSchemaFourMigrationRollsBackAndRetainsSchemaThreeBackup() throws Exception {
        StateDirectory directory=directory(); String id=oldSchema(directory);
        try (var c=DriverManager.getConnection("jdbc:sqlite:"+directory.databaseFile()); var s=c.createStatement()) {
            s.execute("CREATE TABLE pack_passwords (sentinel TEXT)"); s.execute("INSERT INTO pack_passwords VALUES ('untouched')");
        }
        assertEquals("state_migration_failed",assertThrows(StateException.class,() -> StateDatabase.open(directory,CLOCK)).getCode());
        try (var c=DriverManager.getConnection("jdbc:sqlite:"+directory.databaseFile()); var s=c.createStatement()) {
            try (var row=s.executeQuery("PRAGMA user_version")) { assertTrue(row.next()); assertEquals(3,row.getInt(1)); }
            try (var row=s.executeQuery("SELECT sentinel FROM pack_passwords")) { assertTrue(row.next()); assertEquals("untouched",row.getString(1)); }
            try (var row=s.executeQuery("SELECT server_id FROM server_identity")) { assertTrue(row.next()); assertEquals(id,row.getString(1)); }
        }
        try (var files=Files.list(directory.backupsDir())) { assertEquals(1,files.count()); }
    }

    @Test void failedPreMigrationBackupNeverAppliesSchemaFourOrOverwritesExistingArtifact() throws Exception {
        StateDirectory directory=directory(); oldSchema(directory);
        Path collision=directory.backupsDir().resolve("pre-migration-v3-to-v4-"+CLOCK.millis()+".db");
        Files.writeString(collision,"outside-owned-marker");
        assertEquals("state_backup_failed",assertThrows(StateException.class,() -> StateDatabase.open(directory,CLOCK)).getCode());
        assertEquals("outside-owned-marker",Files.readString(collision));
        try (var c=DriverManager.getConnection("jdbc:sqlite:"+directory.databaseFile()); var s=c.createStatement()) {
            try (var row=s.executeQuery("PRAGMA user_version")) { assertTrue(row.next()); assertEquals(3,row.getInt(1)); }
            try (var row=s.executeQuery("SELECT count(*) FROM sqlite_master WHERE name='pack_passwords'")) { assertTrue(row.next()); assertEquals(0,row.getInt(1)); }
        }
    }
}
