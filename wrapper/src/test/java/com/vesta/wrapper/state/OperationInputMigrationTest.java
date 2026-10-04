package com.vesta.wrapper.state;

import com.vesta.wrapper.audit.AuditActor;
import com.vesta.wrapper.audit.AuditLog;
import com.vesta.wrapper.operations.OperationInput;
import com.vesta.wrapper.operations.OperationRequest;
import com.vesta.wrapper.operations.OperationService;
import com.vesta.wrapper.operations.OperationState;
import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.io.TempDir;
import org.junit.jupiter.params.ParameterizedTest;
import org.junit.jupiter.params.provider.ValueSource;

import java.nio.file.Files;
import java.nio.file.Path;
import java.sql.DriverManager;
import java.time.Clock;
import java.time.Instant;
import java.time.ZoneOffset;
import java.util.List;

import static org.junit.jupiter.api.Assertions.*;

class OperationInputMigrationTest {
    @TempDir Path root;
    private static final Clock CLOCK = Clock.fixed(Instant.parse("2026-10-04T12:00:00Z"), ZoneOffset.UTC);
    private static final AuditActor ACTOR = new AuditActor("principal", "device", "https");

    private StateDirectory directory() { return new StateDirectory(root.resolve("state")); }

    private OperationRequest request(String key) {
        return new OperationRequest(ACTOR, "fixture.write", "config/options.txt", "{\"value\":\"requested\"}",
                key, List.of("config"), false, false);
    }

    @Test
    void requestedInputPersistsAcrossReopenWithoutAppearingInPublicOperationOrAudit() {
        StateDirectory directory = directory();
        String id;
        try (StateDatabase db = StateDatabase.open(directory, CLOCK)) {
            AuditLog audit = new AuditLog(db);
            OperationService operations = new OperationService(db, audit, new EventLog(db));
            id = operations.submit(request("persist")).operation().id();
            var input = operations.input(id).orElseThrow();
            assertEquals(OperationInput.InputStatus.AVAILABLE, input.status());
            assertEquals(request("persist").canonicalPayload(), input.canonicalJson());
            assertEquals(request("persist").payloadHash(), input.payloadHash());
            assertFalse(input.toString().contains("requested"));
            assertFalse(operations.get(id).orElseThrow().toString().contains("requested"));
            assertFalse(audit.page(0, 100).toString().contains("requested"));
        }
        try (StateDatabase db = StateDatabase.open(directory, CLOCK)) {
            OperationService operations = new OperationService(db, new AuditLog(db), new EventLog(db));
            assertEquals(request("persist").canonicalPayload(), operations.input(id).orElseThrow().canonicalJson());
            assertEquals(id, operations.claimNext().orElseThrow().id());
        }
    }

    @Test
    void onlyTerminalInputsPurgeAndExactIdempotencyBindingSurvivesPurging() {
        try (StateDatabase db = StateDatabase.open(directory(), CLOCK)) {
            AuditLog audit = new AuditLog(db);
            OperationService operations = new OperationService(db, audit, new EventLog(db));
            String done = operations.submit(request("done")).operation().id();
            assertEquals(done, operations.claimNext().orElseThrow().id());
            operations.markRunning(done);
            operations.succeed(done, "{}");
            String queued = operations.submit(request("queued")).operation().id();
            assertEquals(0, operations.purgeTerminalInputs(CLOCK.instant().minusSeconds(1)));
            assertEquals(1, operations.purgeTerminalInputs(CLOCK.instant().plusSeconds(1)));
            var purged = operations.input(done).orElseThrow();
            assertEquals(OperationInput.InputStatus.PURGED, purged.status());
            assertNull(purged.canonicalJson());
            assertEquals(request("done").payloadHash(), purged.payloadHash());
            assertEquals(OperationInput.InputStatus.AVAILABLE, operations.input(queued).orElseThrow().status());
            assertFalse(operations.submit(request("done")).created());
            assertEquals(done, operations.submit(request("done")).operation().id());
            assertTrue(audit.verifyChain().valid());
        }
    }

    @ParameterizedTest
    @ValueSource(ints = {1, 2})
    void migrationBacksUpLegacySchemaAndNeverFabricatesOrExecutesMissingInputs(int oldVersion) throws Exception {
        StateDirectory directory = directory();
        directory.prepare();
        String serverId;
        try (var c = DriverManager.getConnection("jdbc:sqlite:" + directory.databaseFile());
             var s = c.createStatement()) {
            s.execute("PRAGMA foreign_keys=ON");
            s.execute("PRAGMA journal_mode=WAL");
            for (String sql : Migrations.statements(1, CLOCK.millis())) s.execute(sql);
            if (oldVersion == 2) for (String sql : Migrations.statements(2, CLOCK.millis())) s.execute(sql);
            s.execute("PRAGMA user_version=" + oldVersion);
            try (var row = s.executeQuery("SELECT server_id FROM server_identity")) {
                assertTrue(row.next());
                serverId = row.getString(1);
            }
            for (String id : new String[]{"legacy-queued", "legacy-safe", "legacy-effect"}) {
                boolean effect = id.equals("legacy-effect");
                String state = id.equals("legacy-queued") ? "queued" : "running";
                try (var insert = c.prepareStatement("""
                        INSERT INTO operations (id,server_id,actor_principal_id,device_id,source,type,target,
                        payload_hash,state,requires_confirmation,external_effect_started,external_effect_idempotent,
                        created_at,updated_at) VALUES (?,?,?,?,?,?,?,?,?,0,?,0,?,?)""")) {
                    insert.setString(1, id); insert.setString(2, serverId);
                    insert.setString(3, ACTOR.principalId()); insert.setString(4, ACTOR.deviceId());
                    insert.setString(5, ACTOR.source()); insert.setString(6, "fixture.write");
                    insert.setString(7, "config"); insert.setString(8, request("legacy").payloadHash());
                    insert.setString(9, state); insert.setInt(10, effect ? 1 : 0);
                    insert.setLong(11, CLOCK.millis()); insert.setLong(12, CLOCK.millis()); insert.executeUpdate();
                }
            }
            s.execute("INSERT INTO operation_resources VALUES ('legacy-effect','world')");
            s.execute("INSERT INTO resource_locks VALUES ('world','legacy-effect'," + CLOCK.millis() + ")");
        }
        try (StateDatabase db = StateDatabase.open(directory, CLOCK)) {
            assertEquals(StateDatabase.CURRENT_SCHEMA_VERSION, db.schemaVersion());
            assertEquals(serverId, db.serverId());
            AuditLog audit = new AuditLog(db);
            OperationService operations = new OperationService(db, audit, new EventLog(db));
            var input = operations.input("legacy-queued").orElseThrow();
            assertEquals(OperationInput.InputStatus.UNAVAILABLE_LEGACY, input.status());
            assertNull(input.canonicalJson());
            assertTrue(operations.claimNext().isEmpty(), "legacy payload hashes cannot execute");
            operations.recoverAfterRestart();
            assertEquals(OperationState.FAILED, operations.get("legacy-queued").orElseThrow().state());
            assertEquals(OperationState.FAILED, operations.get("legacy-safe").orElseThrow().state());
            assertEquals(OperationState.RECOVERY_REQUIRED, operations.get("legacy-effect").orElseThrow().state());
            assertEquals(1, db.read(c -> {
                try (var row = c.createStatement().executeQuery("SELECT count(*) FROM resource_locks WHERE resource='world'")) {
                    row.next(); return row.getInt(1);
                }
            }).intValue());
            assertTrue(audit.verifyChain().valid());
        }
        try (var files = Files.list(directory.backupsDir())) {
            var backups = files.toList();
            assertEquals(1, backups.size());
            try (var c = DriverManager.getConnection("jdbc:sqlite:" + backups.get(0));
                 var s = c.createStatement()) {
                try (var row = s.executeQuery("PRAGMA user_version")) {
                    assertTrue(row.next()); assertEquals(oldVersion, row.getInt(1));
                }
                try (var row = s.executeQuery("SELECT count(*) FROM operations")) {
                    assertTrue(row.next()); assertEquals(3, row.getInt(1));
                }
                try (var row = s.executeQuery("SELECT count(*) FROM sqlite_master WHERE name='principals'")) {
                    assertTrue(row.next()); assertEquals(0, row.getInt(1));
                }
            }
        }
    }

    @Test
    void inputRetentionAuditFailureRollsBackInputErasure() {
        try (StateDatabase db = StateDatabase.open(directory(), CLOCK)) {
            OperationService operations = new OperationService(db, new AuditLog(db), new EventLog(db));
            String id = operations.submit(request("purge")).operation().id();
            operations.claimNext(); operations.markRunning(id); operations.succeed(id, "{}");
            db.write(c -> {
                c.createStatement().execute("CREATE TRIGGER audit_failure BEFORE INSERT ON audit_events BEGIN SELECT RAISE(ABORT,'injected'); END");
                return null;
            });
            assertThrows(StateException.class, () -> operations.purgeTerminalInputs(CLOCK.instant().plusSeconds(1)));
            assertEquals(OperationInput.InputStatus.AVAILABLE, operations.input(id).orElseThrow().status());
            assertEquals(request("purge").canonicalPayload(), operations.input(id).orElseThrow().canonicalJson());
        }
    }
}
