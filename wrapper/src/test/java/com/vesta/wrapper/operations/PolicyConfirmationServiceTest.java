package com.vesta.wrapper.operations;

import com.google.gson.JsonObject;
import com.google.gson.JsonParser;
import com.vesta.wrapper.audit.AuditActor;
import com.vesta.wrapper.authorization.Capability;
import com.vesta.wrapper.identity.IdentityRecords.RemoteActor;
import com.vesta.wrapper.state.StateException;
import com.vesta.wrapper.state.StateValues;
import org.junit.jupiter.api.AfterEach;
import org.junit.jupiter.api.BeforeEach;
import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.io.TempDir;
import org.junit.jupiter.params.ParameterizedTest;
import org.junit.jupiter.params.provider.ValueSource;

import java.nio.file.Path;
import java.time.Duration;
import java.util.ArrayList;
import java.util.List;
import java.util.Set;
import java.util.UUID;
import java.util.concurrent.Callable;
import java.util.concurrent.Executors;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.atomic.AtomicInteger;
import java.util.concurrent.atomic.AtomicReference;

import static org.junit.jupiter.api.Assertions.*;

class PolicyConfirmationServiceTest {
    @TempDir Path root;
    private CanonicalOperationFixture fixture;
    private OperationBindings bindings;
    private PolicyConfirmationService gateway;
    private final AtomicReference<String> revision = new AtomicReference<>("generation-A");
    private final AtomicInteger revisionReads = new AtomicInteger();
    private static final String INPUT = "{\"generation\":\"generation-A\"}";

    @BeforeEach void open() {
        fixture = new CanonicalOperationFixture(root.resolve("state"));
        OperationHandler<String> handler = new OperationHandler<>() {
            public String type() { return "fixture.kill"; }
            public HandlerContract contract() { return new HandlerContract(false, true, false); }
            public String parseInput(String json) { return JsonParser.parseString(json).getAsJsonObject().get("generation").getAsString(); }
            public Set<String> requiredResources(String input) { return Set.of("game.generation"); }
            public String execute(String input, OperationContext context) { fail("Confirmation tests must never execute a handler"); return "{}"; }
        };
        bindings = OperationBindings.of(List.of(OperationBinding.of(handler, new OperationBinding.Derivation<>() {
            public String canonicalInput(String input) { var json = new JsonObject(); json.addProperty("generation", input); return json.toString(); }
            public String target(String input) { return input; }
            public List<OperationBinding.Requirement> requirements(String input) {
                return List.of(new OperationBinding.Requirement(Capability.LIFECYCLE_FORCE_KILL, ""));
            }
        })));
        gateway = service((connection, op, input) -> {
            assertFalse(connection.getAutoCommit(), "Revision must be read inside the confirmation write transaction");
            assertEquals(INPUT, input);
            revisionReads.incrementAndGet();
            return revision.get();
        });
    }

    @AfterEach void close() { fixture.close(); }

    private PolicyConfirmationService service(PolicyConfirmationService.SourceRevisionProvider provider) {
        return new PolicyConfirmationService(fixture.operations, fixture.confirmations, fixture.identities.authorization(), bindings, provider);
    }
    private Operation submit() {
        return new OperationSubmissionService(fixture.operations, fixture.identities.authorization(), bindings)
                .submit(fixture.owner, "https", "fixture.kill", INPUT, null).operation();
    }
    private ConfirmationService.IssuedConfirmation issue(Operation op) {
        return gateway.issue(fixture.owner, "https", op.id(), Duration.ofMinutes(1));
    }
    private void sql(String statement) {
        fixture.db.write(c -> { try (var s = c.createStatement()) { s.execute(statement); } return null; });
    }
    private long consumed() {
        return fixture.db.read(c -> { try (var s = c.createStatement(); var r = s.executeQuery("SELECT count(*) FROM confirmations WHERE consumed_at IS NOT NULL")) {
            r.next(); return r.getLong(1);
        } });
    }
    private void assertWaiting(Operation op) {
        assertEquals(OperationState.AWAITING_CONFIRMATION, fixture.operations.get(op.id()).orElseThrow().state());
        assertTrue(fixture.operations.claimNext().isEmpty());
        assertTrue(fixture.audit.verifyChain().valid());
    }
    private void assertSecretFree(String token) {
        assertFalse(fixture.audit.page(0, 100).toString().contains(token));
        fixture.db.read(c -> { try (var s = c.createStatement(); var r = s.executeQuery("SELECT token_hash FROM confirmations")) {
            while (r.next()) { assertNotEquals(token, r.getString(1)); assertEquals(StateValues.sha256Hex(token), r.getString(1)); }
        } return null; });
    }

    @Test void canonicalGatewayDerivesEveryBindingAndReadsServerRevisionInTransaction() {
        Operation op = submit(); var token = issue(op);
        assertEquals(op.id(), token.operationId()); assertEquals(op.type(), token.action());
        assertEquals(op.target(), token.target()); assertEquals(op.payloadHash(), token.payloadHash());
        assertEquals("generation-A", token.sourceRevision()); assertEquals(fixture.owner.principalId(), token.actorPrincipalId());
        assertEquals(fixture.owner.deviceId(), token.deviceId()); assertEquals(1, revisionReads.get());
        assertWaiting(op); assertSecretFree(token.token());
        Operation queued = gateway.confirm(fixture.owner, "https", op.id(), token.token());
        assertEquals(OperationState.QUEUED, queued.state()); assertEquals(2, revisionReads.get()); assertEquals(1, consumed());
        assertThrows(OperationException.class, () -> gateway.confirm(fixture.owner, "https", op.id(), token.token()));
        assertSecretFree(token.token()); assertTrue(fixture.audit.verifyChain().valid());
    }

    @ParameterizedTest @ValueSource(strings = {"revoked", "disabled", "denied", "wrong-device", "unknown", "system"})
    void issueRequiresCurrentCanonicalAuthorityAndRequestingDevice(String change) {
        Operation op = submit(); RemoteActor actor = fixture.owner;
        switch (change) {
            case "revoked" -> fixture.identities.revokeDevice(fixture.owner, fixture.owner.deviceId());
            case "disabled" -> sql("UPDATE principals SET status='disabled'");
            case "denied" -> fixture.deny(Capability.LIFECYCLE_FORCE_KILL);
            case "wrong-device" -> actor = otherDevice();
            case "unknown" -> actor = new RemoteActor("unknown", "unknown");
            case "system" -> actor = new RemoteActor("system", "system");
        }
        RemoteActor presented = actor;
        assertThrows(OperationException.class, () -> gateway.issue(presented, "loopback", op.id(), Duration.ofMinutes(1)));
        assertEquals(0, fixture.count("confirmations")); assertEquals(0, revisionReads.get()); assertWaiting(op);
        assertTrue(fixture.audit.page(0, 100).stream().anyMatch(e -> e.action().equals("confirmation.issue") && e.outcome().equals("rejected")));
    }

    private RemoteActor otherDevice() {
        String device = UUID.randomUUID().toString();
        fixture.db.write(c -> {
            try (var ps = c.prepareStatement("INSERT INTO devices (id,server_id,principal_id,label,public_key_sha256,created_at) VALUES (?,?,?,?,?,?)")) {
                ps.setString(1, device); ps.setString(2, fixture.db.serverId()); ps.setString(3, fixture.owner.principalId());
                ps.setString(4, "Second fixture device"); ps.setString(5, StateValues.sha256Hex(device)); ps.setLong(6, fixture.clock.millis()); ps.executeUpdate();
            }
            return null;
        });
        return new RemoteActor(fixture.owner.principalId(), device);
    }

    @ParameterizedTest @ValueSource(strings = {"revoked", "disabled", "denied", "wrong-device", "unknown", "stale", "expired", "wrong-operation"})
    void currentAuthorityOrRevisionMismatchBurnsTokenWithoutQueuing(String change) {
        Operation op = submit(); var token = issue(op); RemoteActor actor = fixture.owner;
        switch (change) {
            case "revoked" -> fixture.identities.revokeDevice(fixture.owner, fixture.owner.deviceId());
            case "disabled" -> sql("UPDATE principals SET status='disabled'");
            case "denied" -> fixture.deny(Capability.LIFECYCLE_FORCE_KILL);
            case "wrong-device" -> actor = otherDevice();
            case "unknown" -> actor = new RemoteActor("unknown", "unknown");
            case "stale" -> revision.set("generation-B");
            case "expired" -> fixture.clock.advance(Duration.ofMinutes(1));
        }
        RemoteActor presented = actor; String id = change.equals("wrong-operation") ? "other-operation" : op.id();
        assertThrows(OperationException.class, () -> gateway.confirm(presented, "loopback", id, token.token()));
        assertEquals(1, consumed()); assertWaiting(op); assertSecretFree(token.token());
        assertThrows(OperationException.class, () -> gateway.confirm(fixture.owner, "https", op.id(), token.token()));
    }

    @ParameterizedTest @ValueSource(strings = {"target", "payload", "contract", "locks", "input", "unbound"})
    void corruptStoredCanonicalBindingCannotBeIssuedOrConsumed(String mutation) {
        Operation op = submit(); var token = issue(op);
        String statement = switch (mutation) {
            case "target" -> "UPDATE operations SET target='replacement'";
            case "payload" -> "UPDATE operations SET payload_hash='" + "ab".repeat(32) + "'";
            case "contract" -> "UPDATE operations SET requires_confirmation=0";
            case "locks" -> "UPDATE operation_resources SET resource='another'";
            case "input" -> "UPDATE operations SET input_json='{}'";
            default -> "UPDATE operations SET type='unknown.handler'";
        };
        sql(statement);
        assertThrows(OperationException.class, () -> gateway.issue(fixture.owner, "https", op.id(), Duration.ofMinutes(1)));
        assertThrows(OperationException.class, () -> gateway.confirm(fixture.owner, "https", op.id(), token.token()));
        assertEquals(1, fixture.count("confirmations")); assertEquals(1, consumed()); assertWaiting(op);
    }

    @ParameterizedTest @ValueSource(strings = {"null", "empty", "oversized", "control", "exception"})
    void unavailableOrInvalidServerRevisionIsAuditedAndFailsClosed(String failure) {
        Operation op = submit();
        var broken = service((c, operation, input) -> switch (failure) {
            case "null" -> null; case "empty" -> ""; case "oversized" -> "x".repeat(129); case "control" -> "bad\nrevision";
            default -> throw new java.sql.SQLException("fixture detail must not escape");
        });
        assertThrows(OperationException.class, () -> broken.issue(fixture.owner, "https", op.id(), Duration.ofMinutes(1)));
        assertEquals(0, fixture.count("confirmations"));
        var token = issue(op);
        assertThrows(OperationException.class, () -> broken.confirm(fixture.owner, "https", op.id(), token.token()));
        assertEquals(1, consumed()); assertWaiting(op);
        assertFalse(fixture.audit.page(0, 100).toString().contains("fixture detail must not escape"));
    }

    @Test void malformedTokenAndUnknownOperationAreBoundedAuditedRejections() {
        Operation op = submit();
        for (String token : List.of("", "x".repeat(129), "not-a-token")) {
            assertThrows(OperationException.class, () -> gateway.confirm(fixture.owner, "https", op.id(), token));
        }
        assertThrows(OperationException.class, () -> gateway.confirm(fixture.owner, "https", op.id(), null));
        assertThrows(OperationException.class, () -> gateway.issue(fixture.owner, "https", "missing", Duration.ofMinutes(1)));
        assertEquals(0, fixture.count("confirmations")); assertWaiting(op);
    }

    @Test void invalidTtlCannotIssueAnUnboundedOrExpiredConfirmation() {
        Operation op = submit();
        for (Duration ttl : List.of(Duration.ZERO, Duration.ofMillis(-1), Duration.ofMinutes(16))) {
            assertThrows(OperationException.class, () -> gateway.issue(fixture.owner, "https", op.id(), ttl));
        }
        assertThrows(OperationException.class, () -> gateway.issue(fixture.owner, "https", op.id(), null));
        assertEquals(0, fixture.count("confirmations")); assertWaiting(op);
        assertDoesNotThrow(() -> gateway.issue(fixture.owner, "https", op.id(), ConfirmationService.MAX_TTL));
    }

    @Test void auditFailureRollsBackIssueConsumptionAndRejectedTokenBurn() {
        Operation op = submit(); fixture.auditFailure(true);
        assertThrows(StateException.class, () -> issue(op)); assertEquals(0, fixture.count("confirmations"));
        fixture.auditFailure(false); var token = issue(op); fixture.auditFailure(true);
        assertThrows(StateException.class, () -> gateway.confirm(fixture.owner, "https", op.id(), token.token()));
        assertEquals(0, consumed()); assertWaiting(op);
        revision.set("generation-B");
        assertThrows(StateException.class, () -> gateway.confirm(fixture.owner, "https", op.id(), token.token()));
        assertEquals(0, consumed()); assertWaiting(op);
        fixture.auditFailure(false); revision.set("generation-A");
        assertEquals(OperationState.QUEUED, gateway.confirm(fixture.owner, "https", op.id(), token.token()).state());
    }

    @Test void concurrentExactConfirmationsCommitOnlyOneQueueTransition() throws Exception {
        Operation op = submit(); var token = issue(op); var pool = Executors.newFixedThreadPool(6);
        try {
            List<Callable<Boolean>> calls = new ArrayList<>();
            for (int i = 0; i < 12; i++) calls.add(() -> {
                try { gateway.confirm(fixture.owner, "https", op.id(), token.token()); return true; }
                catch (OperationException rejected) { return false; }
            });
            int accepted = 0; for (var result : pool.invokeAll(calls)) if (result.get()) accepted++;
            assertEquals(1, accepted); assertEquals(1, consumed());
            assertEquals(OperationState.QUEUED, fixture.operations.get(op.id()).orElseThrow().state());
            assertTrue(fixture.audit.verifyChain().valid());
        } finally { pool.shutdownNow(); assertTrue(pool.awaitTermination(5, TimeUnit.SECONDS)); }
    }
}
