package com.vesta.wrapper.runtime;

import com.vesta.wrapper.config.WrapperConfig;
import com.vesta.wrapper.console.ConsoleBuffer;
import com.vesta.wrapper.identity.IdentityRecords.RemoteActor;
import com.vesta.wrapper.launch.LaunchSpec;
import com.vesta.wrapper.launch.LaunchSpec.StopMode;
import com.vesta.wrapper.operations.CanonicalOperationFixture;
import com.vesta.wrapper.operations.Operation;
import com.vesta.wrapper.operations.OperationRunner;
import com.vesta.wrapper.operations.OperationState;
import com.vesta.wrapper.operations.lifecycle.LifecycleBindings;
import org.junit.jupiter.api.AfterEach;
import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.io.TempDir;

import java.nio.file.Files;
import java.nio.file.Path;
import java.time.Duration;
import java.util.List;

import static com.vesta.wrapper.runtime.FakeGameFixtures.await;
import static org.junit.jupiter.api.Assertions.*;

/** Canonical lifecycle operations over an explicit native-shape definition with no graceful stop. */
class AgentRuntimeLaunchShapesTest {
    private static final OperationRunner.Settings SETTINGS = new OperationRunner.Settings(2, Duration.ofMillis(10), Duration.ofMillis(100));
    @TempDir Path root;
    private final CanonicalOperationFixture.MutableClock clock = new CanonicalOperationFixture.MutableClock();
    private AgentRuntime runtime;
    private MinecraftServerProcess game;
    private RemoteActor owner;
    private Path work;

    private void open(LaunchSpec spec) throws Exception {
        work = Files.createDirectories(root.resolve("game"));
        var config = new WrapperConfig.Builder().scanRoot(work).minecraftWorkingDirectory(work).minecraftLaunch(spec)
                .durableRuntimeEnabled(true).stateDirectory(root.resolve("state")).build();
        game = new MinecraftServerProcess(config, new ConsoleBuffer(), Duration.ofSeconds(5));
        runtime = AgentRuntime.open(config, game, clock, CanonicalOperationFixture.localAuthority(), SETTINGS);
        owner = CanonicalOperationFixture.bootstrap(runtime.identities(), clock);
    }

    private LaunchSpec nativeSpec(StopMode stop, Path jar) {
        return LaunchSpec.nativeExecutable(FakeGameFixtures.javaExecutable(), List.of("-jar", jar.toString(), "graceful"), stop);
    }

    private Operation submit(String type, String input) {
        var op = runtime.submissions().submit(owner, "https", type, input, null).operation();
        runtime.runner().wake();
        return op;
    }

    private Operation terminal(Operation op) throws Exception {
        await(() -> {
            OperationState state = runtime.operations().get(op.id()).orElseThrow().state();
            return state == OperationState.SUCCEEDED || state == OperationState.FAILED || state == OperationState.RECOVERY_REQUIRED
                    || state == OperationState.CANCELLED;
        });
        return runtime.operations().get(op.id()).orElseThrow();
    }

    private boolean output(String text) {
        return game.consoleBuffer().snapshot(0, 100).lines.stream().anyMatch(line -> line.text.equals(text));
    }

    private int locks() {
        return runtime.database().read(c -> { try (var s = c.createStatement(); var r = s.executeQuery("SELECT count(*) FROM resource_locks")) {
            r.next(); return r.getInt(1);
        } });
    }

    @AfterEach void cleanup() throws Exception {
        if (runtime != null) { runtime.close(); await(runtime::databaseClosed); assertTrue(runtime.runner().fullyStopped()); }
        if (game != null) { game.kill(); await(() -> !game.isRunning()); }
        FakeGameFixtures.deleteTreeWithRetry(root);
    }

    @Test void gracefulStopOperationIsAKnownNoEffectFailureWhileTheProcessStaysRunningAndForceKillStillNeedsConfirmation() throws Exception {
        Path jar = FakeGameFixtures.jar(Files.createDirectories(root.resolve("game")));
        open(nativeSpec(StopMode.UNSUPPORTED, jar));
        Operation started = terminal(submit(LifecycleBindings.START, "{}"));
        assertEquals(OperationState.SUCCEEDED, started.state());
        await(() -> output("FAKE_READY"));
        String generation = game.currentGenerationId(); assertNotNull(generation);

        Operation stop = terminal(submit(LifecycleBindings.STOP, "{\"expected_generation\":\"" + generation + "\"}"));
        assertEquals(OperationState.FAILED, stop.state()); assertEquals("graceful_stop_unsupported", stop.errorCode());
        assertFalse(stop.externalEffectStarted());
        assertEquals(0, locks());
        assertTrue(game.isRunning()); assertEquals(MinecraftServerProcess.State.RUNNING, game.state());
        assertEquals(generation, game.currentGenerationId());
        assertFalse(output("COMMAND:stop"));

        Operation kill = submit(LifecycleBindings.FORCE_KILL, "{\"expected_generation\":\"" + generation + "\"}");
        assertEquals(OperationState.AWAITING_CONFIRMATION, kill.state());
        assertTrue(game.isRunning());
        var token = runtime.confirmations().issue(owner, "https", kill.id(), Duration.ofMinutes(1));
        runtime.confirmations().confirm(owner, "https", kill.id(), token.token());
        runtime.runner().wake();
        assertEquals(OperationState.SUCCEEDED, terminal(kill).state());
        await(() -> !game.isRunning());
        assertEquals(0, locks()); assertTrue(runtime.audit().verifyChain().valid());
    }

    @Test void gracefulStopWorksThroughTheSameOperationWhenTheDefinitionDeclaresConsoleStop() throws Exception {
        Path jar = FakeGameFixtures.jar(Files.createDirectories(root.resolve("game")));
        open(nativeSpec(StopMode.CONSOLE_STOP, jar));
        assertEquals(OperationState.SUCCEEDED, terminal(submit(LifecycleBindings.START, "{}")).state());
        await(() -> output("FAKE_READY"));
        String generation = game.currentGenerationId();
        Operation stop = terminal(submit(LifecycleBindings.STOP, "{\"expected_generation\":\"" + generation + "\"}"));
        assertEquals(OperationState.SUCCEEDED, stop.state()); assertTrue(stop.externalEffectStarted());
        assertFalse(game.isRunning()); assertEquals(0, locks());
    }

    @Test void aMissingExecutableFailsTheStartOperationBeforeAnyExternalEffect() throws Exception {
        open(LaunchSpec.nativeExecutable(root.resolve("no-such-server"), List.of(), StopMode.UNSUPPORTED));
        Operation failed = terminal(submit(LifecycleBindings.START, "{}"));
        assertEquals(OperationState.FAILED, failed.state()); assertEquals("launch_executable_missing", failed.errorCode());
        assertFalse(failed.externalEffectStarted()); assertFalse(game.isRunning()); assertEquals(0, locks());
        assertFalse(Files.exists(work.resolve("starts.txt")));
    }
}
