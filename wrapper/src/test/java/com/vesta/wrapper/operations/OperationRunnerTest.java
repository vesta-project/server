package com.vesta.wrapper.operations;

import com.google.gson.JsonParser;
import com.vesta.wrapper.audit.AuditActor;
import com.vesta.wrapper.audit.AuditLog;
import com.vesta.wrapper.state.EventLog;
import com.vesta.wrapper.state.StateDatabase;
import com.vesta.wrapper.state.StateDirectory;
import com.vesta.wrapper.state.StateException;
import org.junit.jupiter.api.AfterEach;
import org.junit.jupiter.api.BeforeEach;
import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.io.TempDir;
import org.junit.jupiter.params.ParameterizedTest;
import org.junit.jupiter.params.provider.ValueSource;

import java.nio.file.Path;
import java.time.Clock;
import java.time.Duration;
import java.util.ArrayList;
import java.util.List;
import java.util.Set;
import java.util.concurrent.CopyOnWriteArrayList;
import java.util.concurrent.CountDownLatch;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.atomic.AtomicBoolean;
import java.util.concurrent.atomic.AtomicInteger;
import java.util.concurrent.atomic.AtomicReference;
import java.util.function.BooleanSupplier;

import static org.junit.jupiter.api.Assertions.*;

class OperationRunnerTest {
    @TempDir Path root;
    private final AuditActor actor = new AuditActor("principal", "device", "https");
    private final List<OperationRunner> runners = new ArrayList<>();
    private StateDirectory directory;
    private StateDatabase db;
    private AuditLog audit;
    private OperationService operations;
    private static final OperationRunner.Settings SETTINGS = new OperationRunner.Settings(
            2, Duration.ofMillis(20), Duration.ofMillis(150));
    private static final HandlerContract SAFE = new HandlerContract(true, false, true);
    private static final HandlerContract UNSAFE = new HandlerContract(false, false, false);

    @BeforeEach void open() {
        directory = new StateDirectory(root.resolve("state"));
        db = StateDatabase.open(directory, Clock.systemUTC());
        audit = new AuditLog(db);
        operations = new OperationService(db, audit, new EventLog(db));
    }

    @AfterEach void close() throws Exception {
        try {
            for (OperationRunner runner : runners) runner.close();
            for (OperationRunner runner : runners) await(runner::fullyStopped);
        } finally { db.close(); }
    }

    private Operation submit(String type, String key, List<String> locks, HandlerContract contract) {
        return operations.submit(new OperationRequest(actor, type, "config/options.txt", "{\"value\":\"requested\"}",
                key, locks, contract.externalEffectIdempotent(), contract.requiresConfirmation())).operation();
    }

    private OperationRunner runner(List<? extends OperationHandler<?>> handlers, OperationAuthority authority) {
        OperationRunner runner = new OperationRunner(directory, operations, HandlerRegistry.of(handlers), authority, SETTINGS);
        runner.start();
        runners.add(runner);
        return runner;
    }

    @FunctionalInterface private interface Body {
        String execute(String input, OperationContext context) throws Exception;
    }

    private OperationHandler<String> handler(String type, HandlerContract contract, Set<String> locks, Body body) {
        return new OperationHandler<>() {
            @Override public String type() { return type; }
            @Override public HandlerContract contract() { return contract; }
            @Override public String parseInput(String json) {
                return JsonParser.parseString(json).getAsJsonObject().get("value").getAsString();
            }
            @Override public Set<String> requiredResources(String input) { return locks; }
            @Override public String execute(String input, OperationContext context) throws Exception {
                return body.execute(input, context);
            }
        };
    }

    private static void await(BooleanSupplier condition) throws Exception {
        long deadline = System.nanoTime() + TimeUnit.SECONDS.toNanos(8);
        while (!condition.getAsBoolean() && System.nanoTime() < deadline) Thread.sleep(10);
        assertTrue(condition.getAsBoolean(), "condition did not complete within eight seconds");
    }

    private void terminal(String id, OperationState state) throws Exception {
        await(() -> operations.get(id).orElseThrow().state() == state);
    }

    @Test
    void typedHandlerRunsStoredInputAndRechecksAuthorityBeforeEachEffect() throws Exception {
        var observed = new AtomicReference<String>();
        var stages = new CopyOnWriteArrayList<OperationAuthority.Stage>();
        var effects = new AtomicInteger();
        Operation op = submit("fixture.write", "success", List.of("config"), SAFE);
        runner(List.of(handler(op.type(), SAFE, Set.of("config"), (input, ctx) -> {
            observed.set(input);
            ctx.progress(25, "validated");
            ctx.beginExternalEffect(); effects.incrementAndGet();
            ctx.beginExternalEffect(); effects.incrementAndGet();
            return "{\"written\":true}";
        })), (current, stage) -> { stages.add(stage); return OperationAuthority.Decision.allow(); });
        terminal(op.id(), OperationState.SUCCEEDED);
        assertEquals("requested", observed.get());
        assertEquals(2, effects.get());
        assertEquals(List.of(OperationAuthority.Stage.CLAIM, OperationAuthority.Stage.BEFORE_RUN,
                OperationAuthority.Stage.BEFORE_EXTERNAL_EFFECT, OperationAuthority.Stage.BEFORE_EXTERNAL_EFFECT), stages);
        assertEquals("{\"written\":true}", operations.get(op.id()).orElseThrow().resultJson());
        assertTrue(audit.verifyChain().valid());
    }

    @ParameterizedTest
    @ValueSource(strings = {"unknown_handler", "handler_contract_mismatch", "resource_contract_mismatch", "invalid_input"})
    void invalidContractsAndInputFailBeforeAnyExternalEffect(String rejection) throws Exception {
        var effects = new AtomicInteger();
        HandlerContract requested = rejection.equals("handler_contract_mismatch") ? UNSAFE : SAFE;
        List<String> locks = rejection.equals("resource_contract_mismatch") ? List.of() : List.of("config");
        Operation op = submit("fixture.write", rejection, locks, requested);
        if (rejection.equals("invalid_input")) {
            var invalid = new OperationRequest(actor, op.type(), "config", "[]", "invalid-json-shape", locks, true, false);
            operations.requestCancel(actor, op.id());
            op = operations.submit(invalid).operation();
        }
        List<OperationHandler<String>> handlers = rejection.equals("unknown_handler") ? List.of()
                : List.of(handler(op.type(), SAFE, Set.of("config"), (input, ctx) -> {
                    ctx.beginExternalEffect(); effects.incrementAndGet(); return "{}";
                }));
        runner(handlers, (current, stage) -> OperationAuthority.Decision.allow());
        terminal(op.id(), OperationState.FAILED);
        assertEquals(rejection, operations.get(op.id()).orElseThrow().errorCode());
        assertEquals(0, effects.get());
        assertFalse(operations.get(op.id()).orElseThrow().externalEffectStarted());
    }

    @ParameterizedTest
    @ValueSource(strings = {"CLAIM", "BEFORE_RUN", "BEFORE_EXTERNAL_EFFECT", "throw", "null"})
    void currentAuthorityDenialExceptionOrMissingDecisionNeverAllowsAnEffect(String rejection) throws Exception {
        var effects = new AtomicInteger();
        Operation op = submit("fixture.write", rejection, List.of("config"), SAFE);
        runner(List.of(handler(op.type(), SAFE, Set.of("config"), (input, ctx) -> {
            ctx.beginExternalEffect(); effects.incrementAndGet(); return "{}";
        })), (current, stage) -> {
            if (rejection.equals("throw")) throw new IllegalStateException("authority unavailable");
            if (rejection.equals("null")) return null;
            return stage.name().equals(rejection) ? OperationAuthority.Decision.deny("revoked") : OperationAuthority.Decision.allow();
        });
        terminal(op.id(), OperationState.FAILED);
        assertEquals("authority_denied", operations.get(op.id()).orElseThrow().errorCode());
        assertEquals(0, effects.get());
    }

    @Test
    void revocationWhileInputPreparationBlocksIsRecheckedBeforeRun() throws Exception {
        var parsed = new CountDownLatch(1);
        var release = new CountDownLatch(1);
        var allowed = new AtomicBoolean(true);
        var effects = new AtomicInteger();
        Operation op = submit("fixture.write", "revocation-race", List.of("config"), SAFE);
        OperationHandler<String> handler = new OperationHandler<>() {
            @Override public String type() { return op.type(); }
            @Override public HandlerContract contract() { return SAFE; }
            @Override public String parseInput(String json) throws OperationFailure {
                parsed.countDown();
                try {
                    if (!release.await(8, TimeUnit.SECONDS)) throw new OperationFailure("test_timeout", "fixture timed out");
                } catch (InterruptedException e) { throw new OperationFailure("interrupted", "fixture interrupted"); }
                return "requested";
            }
            @Override public Set<String> requiredResources(String input) { return Set.of("config"); }
            @Override public String execute(String input, OperationContext ctx) {
                ctx.beginExternalEffect(); effects.incrementAndGet(); return "{}";
            }
        };
        try {
            runner(List.of(handler), (current, stage) -> allowed.get()
                    ? OperationAuthority.Decision.allow() : OperationAuthority.Decision.deny("revoked"));
            assertTrue(parsed.await(8, TimeUnit.SECONDS));
            allowed.set(false); release.countDown();
            terminal(op.id(), OperationState.FAILED);
            assertEquals("authority_denied", operations.get(op.id()).orElseThrow().errorCode());
            assertEquals(0, effects.get());
        } finally { release.countDown(); }
    }

    @Test
    void uncertainNonIdempotentEffectRequiresRecoveryAndKeepsConflictingWorkQueued() throws Exception {
        Operation broken = submit("fixture.broken", "broken", List.of("config"), UNSAFE);
        Operation blocked = submit("fixture.next", "blocked", List.of("config"), SAFE);
        var nextEffects = new AtomicInteger();
        runner(List.of(handler(broken.type(), UNSAFE, Set.of("config"), (input, ctx) -> {
            ctx.beginExternalEffect(); throw new IllegalStateException("outcome unknown");
        }), handler(blocked.type(), SAFE, Set.of("config"), (input, ctx) -> {
            ctx.beginExternalEffect(); nextEffects.incrementAndGet(); return "{}";
        })), (current, stage) -> OperationAuthority.Decision.allow());
        terminal(broken.id(), OperationState.RECOVERY_REQUIRED);
        assertEquals(OperationState.QUEUED, operations.get(blocked.id()).orElseThrow().state());
        assertEquals(0, nextEffects.get());
        operations.resolveRecovery(actor, broken.id(), OperationState.FAILED, "inspected external effect");
        terminal(blocked.id(), OperationState.SUCCEEDED);
        assertEquals(1, nextEffects.get());
    }

    @Test
    void cancellationAcknowledgesOnlyAtSafePointAndThenUnblocksQueuedWork() throws Exception {
        var entered = new CountDownLatch(1);
        Operation first = submit("fixture.wait", "wait", List.of("config"), SAFE);
        Operation next = submit("fixture.next", "next", List.of("config"), SAFE);
        runner(List.of(handler(first.type(), SAFE, Set.of("config"), (input, ctx) -> {
            entered.countDown();
            while (true) { ctx.checkpoint(); Thread.sleep(10); }
        }), handler(next.type(), SAFE, Set.of("config"), (input, ctx) -> {
            ctx.beginExternalEffect(); return "{}";
        })), (current, stage) -> OperationAuthority.Decision.allow());
        assertTrue(entered.await(8, TimeUnit.SECONDS));
        assertEquals(OperationState.QUEUED, operations.get(next.id()).orElseThrow().state());
        operations.requestCancel(actor, first.id());
        terminal(first.id(), OperationState.CANCELLED);
        terminal(next.id(), OperationState.SUCCEEDED);
    }

    @Test
    void shutdownAtSafePointRequeuesAndAReplacementRunnerCanResume() throws Exception {
        var entered = new CountDownLatch(1);
        Operation op = submit("fixture.wait", "shutdown", List.of("config"), SAFE);
        OperationRunner first = runner(List.of(handler(op.type(), SAFE, Set.of("config"), (input, ctx) -> {
            entered.countDown();
            while (true) { ctx.checkpoint(); Thread.sleep(10); }
        })), (current, stage) -> OperationAuthority.Decision.allow());
        assertTrue(entered.await(8, TimeUnit.SECONDS));
        first.close(); await(first::fullyStopped);
        assertEquals(OperationState.QUEUED, operations.get(op.id()).orElseThrow().state());
        runner(List.of(handler(op.type(), SAFE, Set.of("config"), (input, ctx) -> {
            ctx.beginExternalEffect(); return "{}";
        })), (current, stage) -> OperationAuthority.Decision.allow());
        terminal(op.id(), OperationState.SUCCEEDED);
    }

    @Test
    void shutdownRetainsExclusiveOwnershipUntilAnUncooperativeHandlerActuallyExits() throws Exception {
        var entered = new CountDownLatch(1);
        var release = new CountDownLatch(1);
        Operation op = submit("fixture.stuck", "stuck", List.of("config"), UNSAFE);
        OperationRunner first = runner(List.of(handler(op.type(), UNSAFE, Set.of("config"), (input, ctx) -> {
            ctx.beginExternalEffect(); entered.countDown();
            while (true) {
                try { if (release.await(20, TimeUnit.MILLISECONDS)) break; }
                catch (InterruptedException ignored) { /* Deliberately uncooperative fixture. */ }
            }
            return "{}";
        })), (current, stage) -> OperationAuthority.Decision.allow());
        try {
            assertTrue(entered.await(8, TimeUnit.SECONDS));
            first.close();
            assertFalse(first.fullyStopped());
            assertEquals(OperationState.RUNNING, operations.get(op.id()).orElseThrow().state());
            try (OperationRunner competing = new OperationRunner(directory, operations, HandlerRegistry.of(List.of()),
                    (current, stage) -> OperationAuthority.Decision.allow(), SETTINGS)) {
                assertEquals("runner_lock_held", assertThrows(StateException.class, competing::start).getCode());
            }
            release.countDown();
            terminal(op.id(), OperationState.SUCCEEDED);
            await(first::fullyStopped);
        } finally { release.countDown(); }
    }

    @Test
    void auditFailureDuringRunTransitionStopsClaimsWithoutPerformingAnEffect() throws Exception {
        var effects = new AtomicInteger();
        Operation op = submit("fixture.write", "audit-fail", List.of("config"), SAFE);
        db.write(c -> {
            c.createStatement().execute("CREATE TRIGGER audit_failure BEFORE INSERT ON audit_events WHEN NEW.action='operation.run' BEGIN SELECT RAISE(ABORT,'injected'); END");
            return null;
        });
        OperationRunner runner = runner(List.of(handler(op.type(), SAFE, Set.of("config"), (input, ctx) -> {
            ctx.beginExternalEffect(); effects.incrementAndGet(); return "{}";
        })), (current, stage) -> OperationAuthority.Decision.allow());
        await(() -> !runner.healthy());
        assertEquals(OperationState.VALIDATING, operations.get(op.id()).orElseThrow().state());
        assertEquals(0, effects.get());
        assertTrue(audit.verifyChain().valid());
    }

    @ParameterizedTest
    @ValueSource(booleans = {false, true})
    void oversizedResultIsFailedBeforeEffectButRequiresRecoveryAfterUnknownEffect(boolean beginEffect) throws Exception {
        Operation op = submit("fixture.result", "large", List.of("config"), UNSAFE);
        runner(List.of(handler(op.type(), UNSAFE, Set.of("config"), (input, ctx) -> {
            if (beginEffect) ctx.beginExternalEffect();
            return "x".repeat(OperationService.MAX_RESULT_CHARS + 1);
        })), (current, stage) -> OperationAuthority.Decision.allow());
        terminal(op.id(), beginEffect ? OperationState.RECOVERY_REQUIRED : OperationState.FAILED);
        assertEquals("result_too_large", operations.get(op.id()).orElseThrow().errorCode());
    }

    @Test
    void ownershipTokenIsRequiredAndNeverAppearsInDtosEventsOrAudit() {
        Operation op = submit("fixture.write", "owner", List.of("config"), SAFE);
        String token = "fixture-private-worker-token";
        var claim = operations.claimNext(token).orElseThrow();
        assertFalse(claim.toString().contains(token));
        assertEquals(OperationException.NOT_OWNER, assertThrows(OperationException.class,
                () -> operations.markRunning(op.id())).getCode());
        assertEquals(OperationException.NOT_OWNER, assertThrows(OperationException.class,
                () -> operations.markRunning(op.id(), "another-worker")).getCode());
        operations.markRunning(op.id(), token);
        operations.markExternalEffectStarted(op.id(), token);
        operations.succeed(op.id(), token, "{}");
        assertFalse(operations.get(op.id()).orElseThrow().toString().contains(token));
        assertFalse(audit.page(0, 100).toString().contains(token));
        assertFalse(new EventLog(db).read(0, 100).toString().contains(token));
    }

    @Test
    void registryRejectsDuplicatesAndRunnerCannotLockADifferentDatabaseDirectory() {
        var handler = handler("fixture.write", SAFE, Set.of(), (input, ctx) -> "{}");
        assertThrows(IllegalArgumentException.class, () -> HandlerRegistry.of(List.of(handler, handler)));
        StateDirectory unrelated = new StateDirectory(root.resolve("unrelated"));
        assertEquals("runner_directory_mismatch", assertThrows(StateException.class, () -> new OperationRunner(unrelated, operations,
                HandlerRegistry.of(List.of(handler)), (current, stage) -> OperationAuthority.Decision.allow(), SETTINGS)).getCode());
        assertThrows(NullPointerException.class, () -> new OperationRunner(directory, operations,
                HandlerRegistry.of(List.of(handler)), null, SETTINGS));
    }
}
