package com.vesta.wrapper.operations;

import com.vesta.wrapper.audit.AuditActor;
import com.vesta.wrapper.audit.AuditLog;
import com.vesta.wrapper.state.EventLog;
import com.vesta.wrapper.state.StateDatabase;
import com.vesta.wrapper.state.StateDirectory;
import com.vesta.wrapper.state.StateException;
import org.junit.jupiter.api.BeforeEach;
import org.junit.jupiter.api.AfterEach;
import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.io.TempDir;
import org.junit.jupiter.params.ParameterizedTest;
import org.junit.jupiter.params.provider.ValueSource;

import java.nio.file.Path;
import java.time.Clock;
import java.time.Duration;
import java.time.Instant;
import java.time.ZoneId;
import java.time.ZoneOffset;
import java.util.ArrayList;
import java.util.List;
import java.util.concurrent.Callable;
import java.util.concurrent.Executors;

import static org.junit.jupiter.api.Assertions.*;

class DurableOperationsTest {
    @TempDir Path root;
    private final AuditActor actor = new AuditActor("owner", "device", "https");
    private final MutableClock clock = new MutableClock();
    private StateDatabase db;
    private AuditLog audit;
    private EventLog events;
    private OperationService operations;
    private ConfirmationService confirmations;

    @BeforeEach void open() {
        db = StateDatabase.open(new StateDirectory(root.resolve("state")), clock);
        audit = new AuditLog(db);
        events = new EventLog(db);
        operations = new OperationService(db, audit, events);
        confirmations = new ConfirmationService(db, audit, operations);
    }

    @AfterEach void close() { db.close(); }

    private OperationRequest request(String key, List<String> locks, boolean confirm) {
        return new OperationRequest(actor, "files.delete", "world", "{\"path\":\"world\"}", key, locks, false, confirm);
    }

    @Test
    void idempotencyIsExactServerActorDevicePayloadAndSafetyBinding() {
        var request = request("same", List.of("world", "game", "world"), false);
        var first = operations.submit(request);
        assertTrue(first.created());
        assertEquals(List.of("game", "world"), first.operation().lockResources());
        assertEquals(db.serverId(), first.operation().serverId());
        var repeated = operations.submit(request);
        assertFalse(repeated.created());
        assertEquals(first.operation().id(), repeated.operation().id());
        var variants = List.of(
                new OperationRequest(actor, "files.delete", "world", "changed", "same", request.lockResources(), false, false),
                new OperationRequest(actor, "files.write", "world", request.canonicalPayload(), "same", request.lockResources(), false, false),
                new OperationRequest(actor, request.type(), "another", request.canonicalPayload(), "same", request.lockResources(), false, false),
                new OperationRequest(new AuditActor("owner", "other-device", "https"), request.type(), request.target(), request.canonicalPayload(), "same", request.lockResources(), false, false),
                new OperationRequest(new AuditActor("owner", "device", "local-cli"), request.type(), request.target(), request.canonicalPayload(), "same", request.lockResources(), false, false),
                new OperationRequest(actor, request.type(), request.target(), request.canonicalPayload(), "same", List.of("another-lock"), false, false),
                new OperationRequest(actor, request.type(), request.target(), request.canonicalPayload(), "same", request.lockResources(), true, false),
                new OperationRequest(actor, request.type(), request.target(), request.canonicalPayload(), "same", request.lockResources(), false, true));
        for (var variant : variants) {
            assertEquals(OperationException.IDEMPOTENCY_CONFLICT,
                    assertThrows(OperationException.class, () -> operations.submit(variant)).getCode());
        }
        assertEquals(1, operations.list(0, 100).size());
        assertEquals(variants.size(), audit.page(0, 100).stream().filter(e -> e.outcome().equals("rejected")).count());
        assertTrue(audit.verifyChain().valid());
    }

    @Test
    void concurrentRetriesCommitExactlyOneOperationAndClaimsDoNotOverlap() throws Exception {
        var executor = Executors.newFixedThreadPool(6);
        try {
            List<Callable<String>> tasks = new ArrayList<>();
            for (int i = 0; i < 18; i++) tasks.add(() -> operations.submit(request("race", List.of("world"), false)).operation().id());
            var ids = new java.util.HashSet<String>();
            for (var future : executor.invokeAll(tasks)) ids.add(future.get());
            assertEquals(1, ids.size());
            List<Callable<Boolean>> claims = new ArrayList<>();
            for (int i = 0; i < 6; i++) claims.add(() -> operations.claimNext().isPresent());
            int claimed = 0;
            for (var future : executor.invokeAll(claims)) if (future.get()) claimed++;
            assertEquals(1, claimed);
            assertEquals(1, operations.list(0, 100).size());
            assertTrue(audit.verifyChain().valid());
        } finally {
            executor.shutdownNow();
            assertTrue(executor.awaitTermination(5, java.util.concurrent.TimeUnit.SECONDS));
        }
    }

    @Test
    void blockedOldestWorkReservesResourcesButIndependentWorkCanProceed() {
        String holder = operations.submit(request("holder", List.of("world"), false)).operation().id();
        assertEquals(holder, operations.claimNext().orElseThrow().id());
        operations.markRunning(holder);
        String blocked = operations.submit(request("blocked", List.of("world", "config"), false)).operation().id();
        String overtaker = operations.submit(request("later", List.of("config"), false)).operation().id();
        String independent = operations.submit(request("independent", List.of("logs"), false)).operation().id();
        assertEquals(independent, operations.claimNext().orElseThrow().id());
        assertTrue(operations.claimNext().isEmpty());
        assertEquals(OperationState.QUEUED, operations.get(blocked).orElseThrow().state());
        assertEquals(OperationState.QUEUED, operations.get(overtaker).orElseThrow().state());
        operations.succeed(holder, "{}");
        assertEquals(blocked, operations.claimNext().orElseThrow().id());
        operations.fail(blocked, "validation_failed", "safe failure");
        assertEquals(overtaker, operations.claimNext().orElseThrow().id());
    }

    @Test
    void cancellationReleasesClaimedLocksOnlyAtSafePoint() {
        String id = operations.submit(request("cancel", List.of("world"), false)).operation().id();
        operations.claimNext().orElseThrow();
        assertEquals(OperationState.CANCELLING, operations.requestCancel(actor, id).state());
        operations.submit(request("next", List.of("world"), false));
        assertTrue(operations.claimNext().isEmpty());
        assertThrows(OperationException.class, () -> operations.markRunning(id));
        assertEquals(OperationState.CANCELLED, operations.acknowledgeCancelled(id).state());
        assertTrue(operations.claimNext().isPresent());
        String waiting = operations.submit(request("awaiting", List.of("another"), true)).operation().id();
        assertEquals(OperationState.CANCELLED, operations.requestCancel(actor, waiting).state());
    }

    @Test
    void reopenDoesNotReplayUnknownExternalEffectAndRetainsRecoveryLocks() {
        String unknown = operations.submit(request("unknown", List.of("world"), false)).operation().id();
        operations.claimNext().orElseThrow();
        operations.markRunning(unknown);
        operations.markExternalEffectStarted(unknown);
        String replayable = operations.submit(new OperationRequest(actor, "status.refresh", "status", "{}",
                "safe", List.of("status"), true, false)).operation().id();
        assertEquals(replayable, operations.claimNext().orElseThrow().id());
        operations.markRunning(replayable);
        operations.markExternalEffectStarted(replayable);
        String serverId = db.serverId();
        db.close();
        open();
        assertEquals(serverId, db.serverId());
        assertEquals(2, operations.recoverAfterRestart().size());
        assertEquals(OperationState.RECOVERY_REQUIRED, operations.get(unknown).orElseThrow().state());
        assertEquals(OperationState.QUEUED, operations.get(replayable).orElseThrow().state());
        operations.submit(request("blocked", List.of("world"), false));
        assertEquals(replayable, operations.claimNext().orElseThrow().id());
        assertTrue(operations.claimNext().isEmpty());
        assertThrows(OperationException.class, () -> operations.resolveRecovery(actor, unknown, OperationState.RUNNING, "invalid"));
        operations.resolveRecovery(actor, unknown, OperationState.FAILED, "owner inspected external state");
        assertTrue(operations.claimNext().isPresent());
        assertTrue(audit.verifyChain().valid());
    }

    @ParameterizedTest
    @ValueSource(strings = {"principal", "device", "operation", "action", "target", "payload", "revision", "expired"})
    void confirmationRejectsEveryChangedBindingAndBurnsToken(String changed) {
        Operation op = operations.submit(request("destructive", List.of("world"), true)).operation();
        var issued = confirmations.issue(actor, op.id(), "revision-1", Duration.ofMinutes(1));
        assertFalse(issued.toString().contains(issued.token()));
        AuditActor presented = switch (changed) {
            case "principal" -> new AuditActor("other", "device", "https");
            case "device" -> new AuditActor("owner", "other", "https");
            default -> actor;
        };
        if (changed.equals("expired")) clock.advance(Duration.ofMinutes(1));
        assertEquals(OperationException.CONFIRMATION_REJECTED, assertThrows(OperationException.class, () ->
                confirmations.confirm(presented, changed.equals("operation") ? "other-op" : op.id(), issued.token(),
                        changed.equals("action") ? "other" : op.type(), changed.equals("target") ? "other" : op.target(),
                        changed.equals("payload") ? "different-hash" : op.payloadHash(), changed.equals("revision") ? "revision-2" : "revision-1")).getCode());
        assertThrows(OperationException.class, () -> confirmations.confirm(actor, op.id(), issued.token(), op.type(), op.target(), op.payloadHash(), "revision-1"));
        assertEquals(OperationState.AWAITING_CONFIRMATION, operations.get(op.id()).orElseThrow().state());
        assertTrue(operations.claimNext().isEmpty());
        var fresh = confirmations.issue(actor, op.id(), "revision-2", Duration.ofMinutes(1));
        assertEquals(OperationState.QUEUED, confirmations.confirm(actor, op.id(), fresh.token(), op.type(), op.target(), op.payloadHash(), "revision-2").state());
        assertThrows(OperationException.class, () -> confirmations.confirm(actor, op.id(), fresh.token(), op.type(), op.target(), op.payloadHash(), "revision-2"));
        assertTrue(audit.verifyChain().valid());
        assertFalse(audit.page(0, 100).toString().contains(fresh.token()));
        assertFalse(audit.page(0, 100).toString().contains(issued.token()));
    }

    @Test
    void auditInsertFailureRollsBackStateAndEventsAndDoesNotConsumeConfirmation() {
        Operation op = operations.submit(request("fail-closed", List.of("world"), true)).operation();
        var token = confirmations.issue(actor, op.id(), "r1", Duration.ofMinutes(1));
        long cursor = events.read(0, 100).nextCursor();
        db.write(c -> {
            c.createStatement().execute("CREATE TRIGGER reject_audit BEFORE INSERT ON audit_events BEGIN SELECT RAISE(ABORT, 'injected audit failure'); END");
            return null;
        });
        assertThrows(StateException.class, () -> confirmations.confirm(actor, op.id(), token.token(), op.type(), op.target(), op.payloadHash(), "r1"));
        assertEquals(OperationState.AWAITING_CONFIRMATION, operations.get(op.id()).orElseThrow().state());
        assertTrue(events.read(cursor, 100).events().isEmpty());
        assertThrows(StateException.class, () -> operations.submit(request("other", List.of("config"), false)));
        assertEquals(1, operations.list(0, 100).size());
        db.write(c -> { c.createStatement().execute("DROP TRIGGER reject_audit"); return null; });
        assertEquals(OperationState.QUEUED, confirmations.confirm(actor, op.id(), token.token(), op.type(), op.target(), op.payloadHash(), "r1").state());
    }

    private static final class MutableClock extends Clock {
        private Instant now = Instant.parse("2026-10-04T12:00:00Z");
        void advance(Duration by) { now = now.plus(by); }
        @Override public ZoneId getZone() { return ZoneOffset.UTC; }
        @Override public Clock withZone(ZoneId zone) { return Clock.fixed(now, zone); }
        @Override public Instant instant() { return now; }
    }
}
