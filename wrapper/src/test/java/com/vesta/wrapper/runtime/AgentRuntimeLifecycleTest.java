package com.vesta.wrapper.runtime;

import com.google.gson.JsonParser;
import com.vesta.wrapper.audit.AuditActor;
import com.vesta.wrapper.authorization.Capability;
import com.vesta.wrapper.config.WrapperConfig;
import com.vesta.wrapper.console.ConsoleBuffer;
import com.vesta.wrapper.identity.IdentityRecords.RemoteActor;
import com.vesta.wrapper.operations.*;
import com.vesta.wrapper.operations.lifecycle.LifecycleBindings;
import com.vesta.wrapper.state.StateDirectory;
import com.vesta.wrapper.state.StateException;
import org.junit.jupiter.api.AfterEach;
import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.io.TempDir;
import org.junit.jupiter.params.ParameterizedTest;
import org.junit.jupiter.params.provider.ValueSource;

import java.nio.file.Files;
import java.nio.file.Path;
import java.time.Duration;
import java.util.List;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.atomic.AtomicBoolean;
import java.util.concurrent.atomic.AtomicInteger;
import java.util.function.BooleanSupplier;
import java.util.jar.Attributes;
import java.util.jar.JarEntry;
import java.util.jar.JarOutputStream;
import java.util.jar.Manifest;

import static org.junit.jupiter.api.Assertions.*;

class AgentRuntimeLifecycleTest {
    @TempDir Path root;
    private AgentRuntime runtime;
    private MinecraftServerProcess game;
    private WrapperConfig config;
    private RemoteActor owner;
    private Path jar;
    private final CanonicalOperationFixture.MutableClock clock = new CanonicalOperationFixture.MutableClock();
    private static final OperationRunner.Settings SETTINGS = new OperationRunner.Settings(2, Duration.ofMillis(10), Duration.ofMillis(100));

    private void prepare(String mode) throws Exception {
        Path work = Files.createDirectories(root.resolve("game"));
        jar = work.resolve("fake-game.jar");
        Manifest manifest = new Manifest();
        manifest.getMainAttributes().put(Attributes.Name.MANIFEST_VERSION, "1.0");
        manifest.getMainAttributes().put(Attributes.Name.MAIN_CLASS, FakeMinecraft.class.getName());
        String resource = FakeMinecraft.class.getName().replace('.', '/') + ".class";
        try (var output = new JarOutputStream(Files.newOutputStream(jar), manifest);
             var bytes = FakeMinecraft.class.getClassLoader().getResourceAsStream(resource)) {
            assertNotNull(bytes); output.putNextEntry(new JarEntry(resource)); bytes.transferTo(output); output.closeEntry();
        }
        String executable = System.getProperty("os.name").toLowerCase().contains("windows") ? "java.exe" : "java";
        config = new WrapperConfig.Builder().scanRoot(work).minecraftWorkingDirectory(work)
                .minecraftJavaCommand(Path.of(System.getProperty("java.home"), "bin", executable).toString())
                .minecraftLaunchJar(jar).minecraftJvmArgs(List.of()).minecraftServerArgs(List.of(mode))
                .durableRuntimeEnabled(true).stateDirectory(root.resolve("state")).build();
        game = new MinecraftServerProcess(config, new ConsoleBuffer(), mode.equals("ignore-stop") ? Duration.ofMillis(80) : Duration.ofSeconds(5));
    }
    private void open(String mode) throws Exception {
        prepare(mode);
        runtime = AgentRuntime.open(config, game, clock, CanonicalOperationFixture.localAuthority(), SETTINGS);
        owner = CanonicalOperationFixture.bootstrap(runtime.identities(), clock);
    }
    private Operation submit(String type, String input) {
        var op = runtime.submissions().submit(owner, "https", type, input, null).operation();
        runtime.runner().wake(); return op;
    }
    private Operation terminal(Operation op) throws Exception {
        await(() -> {
            OperationState state = runtime.operations().get(op.id()).orElseThrow().state();
            return state == OperationState.SUCCEEDED || state == OperationState.FAILED || state == OperationState.RECOVERY_REQUIRED || state == OperationState.CANCELLED;
        });
        return runtime.operations().get(op.id()).orElseThrow();
    }
    private String input(String generation) { return "{\"expected_generation\":\"" + generation + "\"}"; }
    private String started() throws Exception {
        int expectedStarts = starts() + 1;
        assertEquals(OperationState.SUCCEEDED, terminal(submit(LifecycleBindings.START, "{}")).state());
        await(() -> {
            try { return starts() == expectedStarts; }
            catch (java.io.IOException notReady) { return false; }
        });
        await(() -> game.consoleBuffer().snapshot(0, 100).lines.stream().filter(line -> line.text.equals("FAKE_READY")).count() >= expectedStarts);
        return game.currentGenerationId();
    }
    private long pid() { return ((Number) game.snapshot().get("pid")).longValue(); }
    private boolean locksEmpty() {
        return runtime.database().read(c -> { try (var s = c.createStatement(); var r = s.executeQuery("SELECT count(*) FROM resource_locks")) {
            r.next(); return r.getInt(1) == 0;
        } });
    }
    private int starts() throws java.io.IOException {
        Path marker = config.minecraftWorkingDirectory.resolve("starts.txt");
        return Files.exists(marker) ? Files.readAllLines(marker).size() : 0;
    }
    private static void await(BooleanSupplier condition) throws Exception {
        long deadline = System.nanoTime() + Duration.ofSeconds(15).toNanos();
        while (!condition.getAsBoolean() && System.nanoTime() < deadline) Thread.sleep(10);
        assertTrue(condition.getAsBoolean(), "Condition did not complete within fifteen seconds");
    }

    @AfterEach void cleanup() throws Exception {
        if (runtime != null) { runtime.close(); await(runtime.runner()::fullyStopped); }
        if (game != null) { game.kill(); await(() -> !game.isRunning()); }
        if (jar != null) {
            long deadline = System.nanoTime() + Duration.ofSeconds(5).toNanos();
            while (Files.exists(jar)) {
                try { Files.delete(jar); }
                catch (java.nio.file.FileSystemException locked) {
                    if (System.nanoTime() >= deadline) throw locked;
                    Thread.sleep(20);
                }
            }
        }
    }

    @Test void realCompositionCreatesCanonicalStateAndAlreadyRunningStartIsANoOp() throws Exception {
        open("graceful"); String generation = started(); long pid = pid();
        assertNotNull(generation); assertFalse(generation.contains(Long.toString(pid)));
        Operation again = terminal(submit(LifecycleBindings.START, "{}"));
        assertEquals(OperationState.SUCCEEDED, again.state()); assertFalse(again.externalEffectStarted());
        assertEquals("already_running", JsonParser.parseString(again.resultJson()).getAsJsonObject().get("outcome").getAsString());
        assertEquals(generation, game.currentGenerationId()); assertEquals(pid, pid()); assertEquals(1, starts());
        assertTrue(runtime.audit().verifyChain().valid()); assertTrue(Files.isRegularFile(runtime.stateDirectory().databaseFile()));
        assertNotEquals(config.minecraftWorkingDirectory, runtime.stateDirectory().path());
    }

    @Test void missingJarIsKnownFailureBeforeAnyExternalEffect() throws Exception {
        open("graceful"); Files.delete(jar);
        Operation failed = terminal(submit(LifecycleBindings.START, "{}"));
        assertEquals(OperationState.FAILED, failed.state()); assertEquals("launch_jar_missing", failed.errorCode());
        assertFalse(failed.externalEffectStarted()); assertFalse(game.isRunning()); assertEquals(0, starts());
    }

    @Test void gracefulStopAndSubsequentStartUseDifferentOpaqueGenerations() throws Exception {
        open("graceful"); String generation = started();
        Operation stopped = terminal(submit(LifecycleBindings.STOP, input(generation)));
        assertEquals(OperationState.SUCCEEDED, stopped.state()); assertFalse(game.isRunning());
        assertNull(game.currentGenerationId()); assertEquals(0, game.snapshot().get("last_exit_code"));
        String replacement = started(); assertNotEquals(generation, replacement); assertEquals(2, starts());
        Operation staleStop = terminal(submit(LifecycleBindings.STOP, input(generation)));
        assertEquals(OperationState.FAILED, staleStop.state()); assertEquals("stale_generation", staleStop.errorCode());
        assertFalse(staleStop.externalEffectStarted()); assertTrue(game.isRunning()); assertEquals(replacement, game.currentGenerationId());
    }

    @Test void stopTimeoutNeverKillsOrStartsAReplacement() throws Exception {
        open("ignore-stop"); String generation = started(); long pid = pid();
        Operation failed = terminal(submit(LifecycleBindings.STOP, input(generation)));
        assertEquals(OperationState.FAILED, failed.state()); assertEquals("stop_timeout", failed.errorCode());
        assertTrue(failed.externalEffectStarted()); assertEquals(MinecraftServerProcess.State.STOPPING, game.state());
        assertTrue(ProcessHandle.of(pid).orElseThrow().isAlive()); assertEquals(generation, game.currentGenerationId());
        assertEquals(1, starts()); assertTrue(locksEmpty());
    }

    @Test void forceKillDoesNothingUntilExactCanonicalConfirmationAndThenKillsOnlyThatGeneration() throws Exception {
        open("ignore-stop"); String generation = started(); long pid = pid();
        Operation op = submit(LifecycleBindings.FORCE_KILL, input(generation));
        assertEquals(OperationState.AWAITING_CONFIRMATION, op.state()); assertTrue(game.isRunning());
        var token = runtime.confirmations().issue(owner, "https", op.id(), Duration.ofMinutes(1));
        assertEquals("game-generation:" + generation, token.sourceRevision());
        assertTrue(game.isRunning());
        runtime.confirmations().confirm(owner, "https", op.id(), token.token()); runtime.runner().wake();
        assertEquals(OperationState.SUCCEEDED, terminal(op).state());
        assertFalse(ProcessHandle.of(pid).map(ProcessHandle::isAlive).orElse(false)); assertEquals(1, starts());
        assertTrue(locksEmpty()); assertTrue(runtime.audit().verifyChain().valid());
    }

    @Test void changedLiveGenerationMakesPreviouslyIssuedKillTokenUnusable() throws Exception {
        open("graceful"); String generation = started();
        Operation op = submit(LifecycleBindings.FORCE_KILL, input(generation));
        var token = runtime.confirmations().issue(owner, "https", op.id(), Duration.ofMinutes(1));
        assertTrue(game.stop()); assertTrue(game.start()); String replacement = game.currentGenerationId();
        assertNotEquals(generation, replacement);
        assertThrows(OperationException.class, () -> runtime.confirmations().confirm(owner, "https", op.id(), token.token()));
        assertTrue(game.isRunning()); assertEquals(replacement, game.currentGenerationId());
        assertEquals(OperationState.AWAITING_CONFIRMATION, runtime.operations().get(op.id()).orElseThrow().state());
    }

    @Test void confirmedKillCannotAffectAReplacementBeforeRunnerExecution() throws Exception {
        prepare("graceful"); assertTrue(game.start()); String original = game.currentGenerationId();
        try (var fixture = new CanonicalOperationFixture(root.resolve("state"))) {
            var bindings = OperationBindings.of(LifecycleBindings.create(game));
            var op = new OperationSubmissionService(fixture.operations, fixture.identities.authorization(), bindings)
                    .submit(fixture.owner, "https", LifecycleBindings.FORCE_KILL, input(original), null).operation();
            var gateway = new PolicyConfirmationService(fixture.operations, fixture.confirmations, fixture.identities.authorization(),
                    bindings, new LifecycleBindings.GenerationRevision(game));
            var token = gateway.issue(fixture.owner, "https", op.id(), Duration.ofMinutes(1));
            gateway.confirm(fixture.owner, "https", op.id(), token.token());
            assertTrue(game.stop()); assertTrue(game.start()); String replacement = game.currentGenerationId();
            var policy = new PolicyOperationAuthority(fixture.operations, fixture.identities.authorization(), bindings);
            try (var runner = new OperationRunner(fixture.directory, fixture.operations, bindings.handlers(), policy, SETTINGS)) {
                runner.start();
                await(() -> fixture.operations.get(op.id()).orElseThrow().state() == OperationState.FAILED);
                var failed = fixture.operations.get(op.id()).orElseThrow();
                assertEquals("stale_generation", failed.errorCode()); assertFalse(failed.externalEffectStarted());
                assertEquals(replacement, game.currentGenerationId()); assertTrue(game.isRunning());
                assertEquals(0, fixture.count("resource_locks"));
            }
        }
    }

    @Test void interruptedNonIdempotentStartIsReconciledWithoutLaunchingOrLosingLocks() throws Exception {
        prepare("graceful"); String operationId; String serverId;
        try (var fixture = new CanonicalOperationFixture(config.stateDirectory)) {
            var bindings = OperationBindings.of(LifecycleBindings.create(game));
            var op = new OperationSubmissionService(fixture.operations, fixture.identities.authorization(), bindings)
                    .submit(fixture.owner, "https", LifecycleBindings.START, "{}", null).operation();
            operationId = op.id(); serverId = fixture.db.serverId();
            fixture.operations.claimNext().orElseThrow(); fixture.operations.markRunning(op.id());
            fixture.operations.markExternalEffectStarted(op.id());
        }
        runtime = AgentRuntime.open(config, game, clock, CanonicalOperationFixture.localAuthority(), SETTINGS);
        assertEquals(serverId, runtime.database().serverId());
        assertEquals(OperationState.RECOVERY_REQUIRED, runtime.operations().get(operationId).orElseThrow().state());
        assertFalse(locksEmpty()); assertFalse(game.isRunning()); assertEquals(0, starts());
    }

    @Test void generationIsRecheckedAtomicallyBeforeForceKillEvenAfterReview() throws Exception {
        prepare("graceful"); assertTrue(game.start()); String old = game.currentGenerationId();
        assertTrue(game.stop()); assertTrue(game.start()); String current = game.currentGenerationId();
        AtomicInteger gates = new AtomicInteger();
        var rejected = assertThrows(MinecraftServerProcess.LifecycleException.class,
                () -> game.killGeneration(old, gates::incrementAndGet, Duration.ofSeconds(1)));
        assertEquals("stale_generation", rejected.getCode()); assertEquals(0, gates.get());
        assertTrue(game.isRunning()); assertEquals(current, game.currentGenerationId());
    }

    @Test void shutdownDuringGracefulStopRetainsLiveGameAndClosesWorkerBeforeDatabase() throws Exception {
        open("ignore-stop"); String generation = started(); long pid = pid();
        submit(LifecycleBindings.STOP, input(generation));
        await(() -> game.consoleBuffer().snapshot(0, 100).lines.stream().anyMatch(line -> line.text.equals("COMMAND:stop")));
        runtime.shutdown(); await(runtime.runner()::fullyStopped);
        assertTrue(ProcessHandle.of(pid).orElseThrow().isAlive()); assertEquals(MinecraftServerProcess.State.STOPPING, game.state());
        assertEquals(generation, game.currentGenerationId());
        assertThrows(StateException.class, () -> runtime.database().read(c -> 1));
    }

    @Test void productionCompositionHasNoLocalProofBackdoorOrAutomaticGameLaunch() throws Exception {
        prepare("graceful"); runtime = AgentRuntime.open(config, game);
        assertFalse(game.isRunning()); assertEquals(0, starts());
        assertThrows(StateException.class, () -> CanonicalOperationFixture.bootstrap(runtime.identities(), clock));
    }

    @Test void secondRuntimeCannotTakeRunnerOwnershipOrStartTheGame() throws Exception {
        open("graceful");
        StateException rejected = assertThrows(StateException.class,
                () -> AgentRuntime.open(config, game, clock, CanonicalOperationFixture.localAuthority(), SETTINGS));
        assertEquals("runner_lock_held", rejected.getCode()); assertTrue(runtime.runner().healthy());
        assertFalse(game.isRunning()); assertEquals(0, starts());
        assertDoesNotThrow(() -> runtime.database().read(c -> 1));
    }

    @Test void linkedStateAncestorIsRefusedBeforeWritingItsTarget() throws Exception {
        prepare("graceful");
        Path outside = Files.createDirectories(root.resolve("outside-state"));
        Path sentinel = outside.resolve("sentinel.txt"); Files.writeString(sentinel, "untouched");
        Path link = root.resolve("redirected-state");
        if (System.getProperty("os.name").toLowerCase().contains("windows")) {
            var process = new ProcessBuilder("cmd.exe", "/c", "mklink", "/J", link.toString(), outside.toString()).redirectErrorStream(true).start();
            if (!process.waitFor(10, TimeUnit.SECONDS)) { process.destroyForcibly(); process.waitFor(); fail("Fixture junction creation timed out"); }
            assertEquals(0, process.exitValue(), new String(process.getInputStream().readAllBytes(), java.nio.charset.StandardCharsets.UTF_8));
        } else {
            Files.createSymbolicLink(link, outside);
        }
        try {
            var redirected = new WrapperConfig.Builder().scanRoot(config.scanRoot).minecraftWorkingDirectory(config.minecraftWorkingDirectory)
                    .minecraftLaunchJar(jar).durableRuntimeEnabled(true).stateDirectory(link.resolve("nested")).build();
            var failure = assertThrows(StateException.class, () -> AgentRuntime.open(redirected, game));
            assertEquals("state_dir_linked", failure.getCode()); assertFalse(Files.exists(outside.resolve("nested")));
            assertEquals("untouched", Files.readString(sentinel)); assertFalse(game.isRunning());
        } finally { Files.delete(link); }
    }

    @ParameterizedTest @ValueSource(strings = {"inside-game", "contains-game", "corrupt"})
    void unsafeStateFailsBeforeAnyGameLaunch(String kind) throws Exception {
        prepare("graceful");
        Path state = switch (kind) {
            case "inside-game" -> config.minecraftWorkingDirectory.resolve("state");
            case "contains-game" -> root;
            default -> root.resolve("corrupt-state");
        };
        if (kind.equals("corrupt")) {
            new StateDirectory(state).prepare(); Files.writeString(state.resolve(StateDirectory.DATABASE_FILE), "not SQLite");
        }
        var unsafe = new WrapperConfig.Builder().scanRoot(config.scanRoot).minecraftWorkingDirectory(config.minecraftWorkingDirectory)
                .minecraftLaunchJar(jar).durableRuntimeEnabled(true).stateDirectory(state).build();
        assertThrows(StateException.class, () -> AgentRuntime.open(unsafe, game));
        assertFalse(game.isRunning()); assertEquals(0, starts());
    }

    @ParameterizedTest @ValueSource(strings = {"[]", "null", "{\"unknown\":true}", "{} {}", "{\"expected_generation\":123}", "{\"expected_generation\":\"gen_a\",\"expected_generation\":\"gen_b\"}"})
    void strictLifecycleParsingRejectsMalformedRequestsBeforePersistence(String json) throws Exception {
        open("graceful");
        for (String type : List.of(LifecycleBindings.START, LifecycleBindings.STOP, LifecycleBindings.FORCE_KILL)) {
            assertThrows(OperationException.class, () -> submit(type, json));
        }
        assertTrue(runtime.operations().list(0, 100).isEmpty()); assertFalse(game.isRunning());
    }

    @Test void revocationImmediatelyBeforeExternalEffectPreventsRealProcessStart() throws Exception {
        prepare("graceful");
        try (var fixture = new CanonicalOperationFixture(root.resolve("state"))) {
            var bindings = OperationBindings.of(LifecycleBindings.create(game));
            var policy = new PolicyOperationAuthority(fixture.operations, fixture.identities.authorization(), bindings);
            AtomicBoolean revoked = new AtomicBoolean();
            OperationAuthority authority = (op, stage) -> {
                if (stage == OperationAuthority.Stage.BEFORE_EXTERNAL_EFFECT && revoked.compareAndSet(false, true))
                    fixture.identities.revokeDevice(fixture.owner, fixture.owner.deviceId());
                return policy.recheck(op, stage);
            };
            var op = new OperationSubmissionService(fixture.operations, fixture.identities.authorization(), bindings)
                    .submit(fixture.owner, "https", LifecycleBindings.START, "{}", null).operation();
            try (var runner = new OperationRunner(fixture.directory, fixture.operations, bindings.handlers(), authority, SETTINGS)) {
                runner.start();
                await(() -> fixture.operations.get(op.id()).orElseThrow().state() == OperationState.FAILED);
                assertTrue(revoked.get()); assertFalse(fixture.operations.get(op.id()).orElseThrow().externalEffectStarted());
                assertFalse(game.isRunning()); assertEquals(0, starts()); assertTrue(fixture.count("resource_locks") == 0);
            }
        }
    }

    @Test void denyOverrideAndLoopbackLabelCannotAuthorizeLifecycleSubmission() throws Exception {
        open("graceful");
        assertThrows(OperationException.class, () -> runtime.submissions().submit(new RemoteActor("system", "system"), "loopback", LifecycleBindings.START, "{}", null));
        runtime.database().write(c -> {
            try (var ps = c.prepareStatement("INSERT INTO principal_overrides (id,principal_id,capability,scope,effect,granted_by,created_at) VALUES ('deny-start',?,?,'','deny','fixture',?)")) {
                ps.setString(1, owner.principalId()); ps.setString(2, Capability.LIFECYCLE_START.id()); ps.setLong(3, clock.millis()); ps.executeUpdate();
            } return null;
        });
        assertThrows(OperationException.class, () -> runtime.submissions().submit(owner, "loopback", LifecycleBindings.START, "{}", null));
        assertFalse(game.isRunning()); assertTrue(runtime.operations().list(0, 100).isEmpty());
    }
}
