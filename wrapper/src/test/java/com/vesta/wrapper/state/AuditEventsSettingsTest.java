package com.vesta.wrapper.state;

import com.vesta.wrapper.audit.AuditActor;
import com.vesta.wrapper.audit.AuditLog;
import com.vesta.wrapper.audit.AuditRecord;
import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.io.TempDir;
import org.junit.jupiter.params.ParameterizedTest;
import org.junit.jupiter.params.provider.ValueSource;

import java.nio.file.Path;
import java.time.Clock;
import java.time.Instant;
import java.time.ZoneOffset;
import java.util.Map;

import static org.junit.jupiter.api.Assertions.*;

class AuditEventsSettingsTest {
    @TempDir Path root;
    private static final Clock CLOCK = Clock.fixed(Instant.parse("2026-10-04T12:00:00Z"), ZoneOffset.UTC);
    private final AuditActor actor = new AuditActor("owner", "device", "local-cli");
    private StateDatabase open() { return StateDatabase.open(new StateDirectory(root.resolve("state")), CLOCK); }

    @Test
    void auditIsAppendOnlyAndModifiedRecordDetectedIncludingOnServiceOpen() {
        try (StateDatabase db = open()) {
            AuditLog audit = new AuditLog(db);
            audit.append(new AuditRecord(actor, "files.write", "config.txt", "ok", null, Map.of("revision", "1")));
            audit.append(AuditRecord.of(actor, "files.read", "config.txt", "ok", null));
            assertTrue(audit.verifyChain().valid());
            assertEquals(2, audit.verifyChain().checkedEntries());
            assertEquals(1, audit.page(0, 1).size());
            assertEquals(1, audit.page(1, 1).size());
            assertThrows(StateException.class, () -> db.write(c -> {
                c.createStatement().executeUpdate("UPDATE audit_events SET outcome = 'changed' WHERE seq = 1");
                return null;
            }));
            assertThrows(StateException.class, () -> db.write(c -> {
                c.createStatement().executeUpdate("DELETE FROM audit_events WHERE seq = 1");
                return null;
            }));
            // Simulate a local administrator bypassing the append-only trigger, not a remote API.
            db.write(c -> {
                c.createStatement().execute("DROP TRIGGER audit_events_no_update");
                c.createStatement().executeUpdate("UPDATE audit_events SET outcome = 'changed' WHERE seq = 1");
                return null;
            });
            assertFalse(audit.verifyChain().valid());
            assertEquals(1, audit.verifyChain().firstBrokenSeq());
            assertEquals("audit_chain_corrupt", assertThrows(StateException.class, () -> new AuditLog(db)).getCode());
        }
    }

    @ParameterizedTest
    @ValueSource(strings = {"password", "apiToken", "privateKey", "secret", "credential", "cookie", "authorization", "command"})
    void accidentalSecretMetadataKeysAreRejected(String key) {
        assertThrows(StateException.class, () -> new AuditRecord(actor, "change", "server", "ok", null, Map.of(key, "secret-value")));
    }

    @Test
    void auditMutationRollsBackWithCallerAndMetadataIsCanonical() {
        try (StateDatabase db = open()) {
            AuditLog audit = new AuditLog(db);
            var left = new java.util.LinkedHashMap<String, String>();
            left.put("z", "last"); left.put("a", "first");
            var right = new java.util.LinkedHashMap<String, String>();
            right.put("a", "first"); right.put("z", "last");
            assertEquals(new AuditRecord(actor, "change", "server", "ok", null, left).metadata(),
                    new AuditRecord(actor, "change", "server", "ok", null, right).metadata());
            assertThrows(IllegalStateException.class, () -> db.write(c -> {
                audit.append(c, AuditRecord.of(actor, "change", "server", "ok", null));
                throw new IllegalStateException("rollback");
            }));
            assertTrue(audit.page(0, 10).isEmpty());
        }
    }

    @Test
    void desiredAndEffectiveRevisionsDoNotPretendASettingWasApplied() {
        try (StateDatabase db = open()) {
            SettingsStore settings = new SettingsStore(db, new AuditLog(db));
            var first = settings.setDesired(actor, "game.memory", "4G", SettingsStore.ApplyMode.ON_RESTART);
            assertTrue(first.pending());
            assertNull(first.effectiveValue());
            assertEquals(0, first.effectiveRevision());
            var applied = settings.markEffective(actor, first.key(), first.desiredRevision());
            assertFalse(applied.pending());
            var next = settings.setDesired(actor, first.key(), "6G", SettingsStore.ApplyMode.MANUAL);
            assertEquals(2, next.desiredRevision());
            assertEquals("4G", next.effectiveValue());
            assertTrue(next.pending());
            assertEquals("stale_revision", assertThrows(StateException.class,
                    () -> settings.markEffective(actor, next.key(), 1)).getCode());
            assertEquals(1, settings.pending().size());
            var immediate = settings.setDesired(actor, "console.visible", "true", SettingsStore.ApplyMode.IMMEDIATE);
            assertTrue(immediate.pending(), "IMMEDIATE still requires the adapter to acknowledge application");
        }
        try (StateDatabase db = open()) {
            var persisted = new SettingsStore(db, new AuditLog(db)).get("game.memory").orElseThrow();
            assertEquals("6G", persisted.desiredValue());
            assertEquals("4G", persisted.effectiveValue());
        }
    }

    @Test
    void settingMutationFailsClosedWhenAuditStorageFails() {
        try (StateDatabase db = open()) {
            SettingsStore settings = new SettingsStore(db, new AuditLog(db));
            db.write(c -> {
                c.createStatement().execute("CREATE TRIGGER reject_audit BEFORE INSERT ON audit_events BEGIN SELECT RAISE(ABORT, 'injected failure'); END");
                return null;
            });
            assertThrows(StateException.class, () -> settings.setDesired(actor, "game.memory", "4G", SettingsStore.ApplyMode.ON_RESTART));
            assertTrue(settings.get("game.memory").isEmpty());
        }
    }

    @Test
    void cursorPaginationRetentionGapsAndEmptyStreamWatermarkAreDurable() {
        try (StateDatabase db = open()) {
            EventLog events = new EventLog(db);
            for (int i = 0; i < 3; i++) db.write(c -> events.append(c, "test.changed", null, "{}"));
            var first = events.read(0, 1);
            assertEquals(1, first.events().size());
            assertFalse(first.gap());
            assertEquals(db.serverId(), first.events().get(0).serverId());
            assertEquals(2, events.read(first.nextCursor(), 500).events().size());
            assertEquals(3, events.pruneOlderThan(CLOCK.instant().plusSeconds(1)));
            var gap = events.read(0, 10);
            assertTrue(gap.gap());
            assertTrue(gap.events().isEmpty());
            assertEquals(3, gap.nextCursor());
            assertEquals(0, gap.oldestAvailableCursor());
            assertFalse(events.read(3, 10).gap());
            db.write(c -> events.append(c, "test.new", null, "{}"));
            assertEquals(4, events.read(3, 10).events().get(0).cursor());
        }
        try (StateDatabase db = open()) {
            assertTrue(new EventLog(db).read(0, 10).gap());
        }
    }

    @Test
    void clockRollbackDoesNotPruneNonexpiredEventsOrCreateHoles() {
        try (StateDatabase db = open()) {
            EventLog events = new EventLog(db);
            for (int i = 0; i < 3; i++) db.write(c -> events.append(c, "test.changed", null, "{}"));
            db.write(c -> {
                c.createStatement().executeUpdate("UPDATE events SET at = 100 WHERE cursor IN (1, 3)");
                c.createStatement().executeUpdate("UPDATE events SET at = 1000 WHERE cursor = 2");
                return null;
            });
            assertEquals(1, events.pruneOlderThan(Instant.ofEpochMilli(500)));
            var page = events.read(0, 10);
            assertTrue(page.gap());
            assertEquals(java.util.List.of(2L, 3L), page.events().stream().map(EventLog.StoredEvent::cursor).toList());
        }
    }
}
