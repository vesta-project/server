package com.vesta.wrapper.state;

import com.vesta.wrapper.audit.AuditActor;
import com.vesta.wrapper.audit.AuditLog;
import com.vesta.wrapper.audit.AuditRecord;
import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.io.TempDir;

import java.nio.file.Files;
import java.nio.file.Path;
import java.time.Clock;
import java.util.ArrayList;
import java.util.concurrent.Callable;
import java.util.concurrent.CountDownLatch;
import java.util.concurrent.Executors;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.TimeoutException;

import static org.junit.jupiter.api.Assertions.*;

class StateDatabaseLifecycleTest {
    @TempDir Path root;

    @Test
    void concurrentShortLivedConnectionsKeepCompanionsStableAndReopenWithAllAuditedWrites() throws Exception {
        StateDirectory directory = new StateDirectory(root.resolve("state"));
        String serverId = null;
        for (int round = 0; round < 3; round++) {
            try (StateDatabase db = StateDatabase.open(directory, Clock.systemUTC())) {
                if (serverId == null) serverId = db.serverId();
                assertEquals(serverId, db.serverId());
                if (round == 0) db.write(c -> {
                    c.createStatement().execute("CREATE TABLE fixture_counter (id INTEGER PRIMARY KEY, value INTEGER NOT NULL) STRICT");
                    c.createStatement().execute("INSERT INTO fixture_counter VALUES (1,0)");
                    return null;
                });
                AuditLog audit = new AuditLog(db);
                var start = new CountDownLatch(1);
                var pool = Executors.newFixedThreadPool(6);
                try {
                    var work = new ArrayList<Callable<Void>>();
                    for (int worker = 0; worker < 6; worker++) {
                        boolean writer = worker % 2 == 0;
                        work.add(() -> {
                            assertTrue(start.await(5, TimeUnit.SECONDS));
                            for (int iteration = 0; iteration < 60; iteration++) {
                                if (writer) db.write(c -> {
                                    c.createStatement().executeUpdate("UPDATE fixture_counter SET value=value+1 WHERE id=1");
                                    audit.append(c, AuditRecord.of(AuditActor.system("fixture"), "fixture.increment", "counter", "ok", null));
                                    return null;
                                });
                                else db.read(c -> {
                                    try (var rows = c.createStatement().executeQuery("SELECT value FROM fixture_counter")) {
                                        assertTrue(rows.next()); assertTrue(rows.getInt(1) >= 0);
                                    }
                                    return null;
                                });
                                assertTrue(Files.isRegularFile(directory.databaseFile().resolveSibling("vesta-state.db-wal")));
                                assertTrue(Files.isRegularFile(directory.databaseFile().resolveSibling("vesta-state.db-shm")));
                            }
                            return null;
                        });
                    }
                    var futures = work.stream().map(pool::submit).toList();
                    start.countDown();
                    for (var future : futures) future.get(30, TimeUnit.SECONDS);
                } finally {
                    start.countDown(); pool.shutdownNow(); assertTrue(pool.awaitTermination(5, TimeUnit.SECONDS));
                }
                int expected = (round + 1) * 180;
                int actual = db.read(c -> {
                    try (var rows = c.createStatement().executeQuery("SELECT value FROM fixture_counter")) {
                        rows.next(); return rows.getInt(1);
                    }
                });
                assertEquals(expected, actual);
                int audited = db.read(c -> {
                    try (var rows = c.createStatement().executeQuery("SELECT count(*) FROM audit_events WHERE action='fixture.increment'")) {
                        rows.next(); return rows.getInt(1);
                    }
                });
                assertEquals(expected, audited);
                assertTrue(audit.verifyChain().valid());
            }
            assertFalse(Files.exists(directory.databaseFile().resolveSibling("vesta-state.db-wal")));
            assertFalse(Files.exists(directory.databaseFile().resolveSibling("vesta-state.db-shm")));
        }
    }

    @Test
    void closeWaitsForAnInFlightSnapshotAndThenRejectsAllNewCalls() throws Exception {
        StateDatabase db = StateDatabase.open(new StateDirectory(root.resolve("state")), Clock.systemUTC());
        var entered = new CountDownLatch(1);
        var release = new CountDownLatch(1);
        var closeStarted = new CountDownLatch(1);
        var pool = Executors.newFixedThreadPool(2);
        try {
            var reader = pool.submit(() -> db.read(c -> {
                try (var rows = c.createStatement().executeQuery("SELECT server_id FROM server_identity")) {
                    assertTrue(rows.next());
                    entered.countDown();
                    try { assertTrue(release.await(8, TimeUnit.SECONDS)); }
                    catch (InterruptedException e) { Thread.currentThread().interrupt(); throw new AssertionError(e); }
                    return rows.getString(1);
                }
            }));
            assertTrue(entered.await(5, TimeUnit.SECONDS));
            var closer = pool.submit(() -> { closeStarted.countDown(); db.close(); });
            assertTrue(closeStarted.await(5, TimeUnit.SECONDS));
            assertThrows(TimeoutException.class, () -> closer.get(150, TimeUnit.MILLISECONDS),
                    "close must not dispose the anchor while a read callback is running");
            release.countDown();
            assertEquals(db.serverId(), reader.get(5, TimeUnit.SECONDS));
            closer.get(5, TimeUnit.SECONDS);
            assertEquals("state_closed", assertThrows(StateException.class, () -> db.read(c -> true)).getCode());
            assertEquals("state_closed", assertThrows(StateException.class, () -> db.write(c -> true)).getCode());
            assertDoesNotThrow(db::close);
        } finally {
            release.countDown(); pool.shutdownNow(); assertTrue(pool.awaitTermination(5, TimeUnit.SECONDS)); db.close();
        }
    }

    @Test
    void closingInsideEitherCallbackRejectsWithoutDeadlockOrClosingTheDatabase() {
        try (StateDatabase db = StateDatabase.open(new StateDirectory(root.resolve("state")), Clock.systemUTC())) {
            assertEquals("close_in_callback", assertThrows(StateException.class, () -> db.read(c -> { db.close(); return null; })).getCode());
            assertEquals("close_in_callback", assertThrows(StateException.class, () -> db.write(c -> { db.close(); return null; })).getCode());
            assertEquals(StateDatabase.CURRENT_SCHEMA_VERSION, db.schemaVersion());
            assertEquals(db.serverId(), db.read(c -> {
                try (var rows = c.createStatement().executeQuery("SELECT server_id FROM server_identity")) {
                    rows.next(); return rows.getString(1);
                }
            }));
        }
    }
}
