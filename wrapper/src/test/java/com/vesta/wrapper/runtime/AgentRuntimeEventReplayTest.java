package com.vesta.wrapper.runtime;

import com.vesta.wrapper.config.WrapperConfig;
import com.vesta.wrapper.console.ConsoleBuffer;
import com.vesta.wrapper.identity.IdentityRecords.RemoteActor;
import com.vesta.wrapper.operations.CanonicalOperationFixture;
import com.vesta.wrapper.operations.OperationEventReplayService.Event;
import com.vesta.wrapper.operations.OperationEventReplayService.Page;
import com.vesta.wrapper.operations.OperationRunner;
import com.vesta.wrapper.operations.OperationState;
import com.vesta.wrapper.operations.lifecycle.LifecycleBindings;
import com.vesta.wrapper.state.StateException;
import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.io.TempDir;

import java.nio.file.Files;
import java.nio.file.Path;
import java.time.Duration;
import java.util.List;
import java.util.function.BooleanSupplier;

import static org.junit.jupiter.api.Assertions.*;

class AgentRuntimeEventReplayTest {
    @TempDir Path root;
    private static final OperationRunner.Settings SETTINGS = new OperationRunner.Settings(1, Duration.ofMillis(10), Duration.ofMillis(100));

    private WrapperConfig config() throws Exception {
        Path game = Files.createDirectories(root.resolve("game"));
        return new WrapperConfig.Builder().scanRoot(game).minecraftWorkingDirectory(game)
                .minecraftLaunchJar(game.resolve("never-created-game.jar"))
                .durableRuntimeEnabled(true).stateDirectory(root.resolve("state")).build();
    }

    private static void await(BooleanSupplier condition) throws Exception {
        long deadline = System.nanoTime() + Duration.ofSeconds(15).toNanos();
        while (!condition.getAsBoolean() && System.nanoTime() < deadline) Thread.sleep(10);
        assertTrue(condition.getAsBoolean(), "Condition did not complete within fifteen seconds");
    }

    private static void failedStart(AgentRuntime runtime, RemoteActor owner) throws Exception {
        var op = runtime.submissions().submit(owner, "https", LifecycleBindings.START, "{}", null).operation();
        runtime.runner().wake();
        await(() -> runtime.operations().get(op.id()).orElseThrow().state() == OperationState.FAILED);
    }

    @Test void productionCompositionHasNoOwnerSoEveryReplayCallIsForbiddenAndNoGameOrRouteExists() throws Exception {
        var config = config(); var game = new MinecraftServerProcess(config, new ConsoleBuffer());
        try (var runtime = AgentRuntime.open(config, game)) {
            assertNotNull(runtime.operationEvents());
            for (RemoteActor caller : new RemoteActor[]{null, new RemoteActor("system", "local"), new RemoteActor("unknown", "unknown")}) {
                assertEquals("forbidden", assertThrows(StateException.class, () -> runtime.operationEvents().openReplay(caller, null, 10)).getCode());
                assertEquals("forbidden", assertThrows(StateException.class, () -> runtime.operationEvents().currentCheckpoint(caller)).getCode());
            }
            assertFalse(game.isRunning()); assertEquals(4, runtime.database().schemaVersion());
            int principals = runtime.database().read(c -> { try (var s = c.createStatement(); var r = s.executeQuery("SELECT count(*) FROM principals")) { r.next(); return r.getInt(1); } });
            assertEquals(0, principals);
        }
        assertFalse(game.isRunning()); assertFalse(Files.exists(config.minecraftLaunchJar));
    }

    @Test void trustedBootstrapReplaysRealLifecycleFailureWithoutErrorTextAndCheckpointsSurviveRestart() throws Exception {
        var config = config(); var game = new MinecraftServerProcess(config, new ConsoleBuffer());
        var clock = new CanonicalOperationFixture.MutableClock();
        RemoteActor owner; String checkpoint; String serverId; long lastBeforeRestart;
        var runtime = AgentRuntime.open(config, game, clock, CanonicalOperationFixture.localAuthority(), SETTINGS);
        try {
            owner = CanonicalOperationFixture.bootstrap(runtime.identities(), clock); serverId = runtime.database().serverId();
            failedStart(runtime, owner);
            Page page = runtime.operationEvents().openReplay(owner, null, 100);
            assertTrue(page.complete());
            assertEquals("operation.created", page.events().get(0).type());
            assertEquals(OperationState.QUEUED, page.events().get(0).state());
            assertEquals(OperationState.FAILED, page.events().get(page.events().size() - 1).state());
            assertTrue(page.events().stream().map(Event::operationId).distinct().count() == 1);
            assertFalse(page.toString().contains("launch_jar_missing")); assertFalse(page.toString().contains(LifecycleBindings.START));
            assertFalse(game.isRunning()); assertFalse(Files.exists(config.minecraftLaunchJar));
            checkpoint = page.nextCursor();
            lastBeforeRestart = Long.parseLong(page.events().get(page.events().size() - 1).eventId());
        } finally {
            runtime.close(); await(runtime::databaseClosed);
        }
        runtime = AgentRuntime.open(config, game, clock, CanonicalOperationFixture.localAuthority(), SETTINGS);
        try {
            assertEquals(serverId, runtime.database().serverId());
            Page quiet = runtime.operationEvents().openReplay(owner, checkpoint, 100);
            assertTrue(quiet.events().isEmpty()); assertTrue(quiet.complete());
            failedStart(runtime, owner);
            Page later = runtime.operationEvents().openReplay(owner, checkpoint, 100);
            assertFalse(later.events().isEmpty());
            assertEquals(List.of(OperationState.QUEUED), later.events().subList(0, 1).stream().map(Event::state).toList());
            assertEquals(OperationState.FAILED, later.events().get(later.events().size() - 1).state());
            assertEquals(lastBeforeRestart + 1, Long.parseLong(later.events().get(0).eventId()));
            assertTrue(runtime.audit().verifyChain().valid()); assertFalse(game.isRunning());
        } finally {
            runtime.close(); await(runtime::databaseClosed);
        }
    }
}
