package com.vesta.wrapper.server;

import com.google.gson.JsonObject;
import com.google.gson.JsonParser;
import com.vesta.sync.VestaSync;
import com.vesta.sync.auth.CapabilityEngine;
import com.vesta.sync.auth.LinkCodeService;
import com.vesta.sync.auth.MinecraftSessionChallengeService;
import com.vesta.sync.auth.RealtimeTicketService;
import com.vesta.wrapper.auth.AuthService;
import com.vesta.wrapper.config.WrapperConfig;
import com.vesta.wrapper.console.ConsoleBuffer;
import com.vesta.wrapper.launch.LaunchSpec;
import com.vesta.wrapper.launch.LaunchSpec.StopMode;
import com.vesta.wrapper.runtime.FakeGameFixtures;
import com.vesta.wrapper.runtime.MinecraftServerProcess;
import com.vesta.wrapper.security.WhitelistService;
import org.junit.jupiter.api.AfterEach;
import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.io.TempDir;

import java.net.URI;
import java.net.http.HttpClient;
import java.net.http.HttpRequest;
import java.net.http.HttpResponse;
import java.nio.file.Files;
import java.nio.file.Path;
import java.time.Duration;
import java.util.List;
import java.util.Optional;

import static com.vesta.wrapper.runtime.FakeGameFixtures.await;
import static org.junit.jupiter.api.Assertions.*;

/** The legacy (non-durable) power route must not claim a stop that its launch definition cannot perform. */
class LegacyPowerRouteLaunchTest {
    @TempDir Path root;
    private MinecraftServerProcess game;
    private VestaHttpServer server;
    private Path work;

    private void open(StopMode stop) throws Exception {
        work = Files.createDirectories(root.resolve("game"));
        Path jar = FakeGameFixtures.jar(work);
        var spec = LaunchSpec.nativeExecutable(FakeGameFixtures.javaExecutable(), List.of("-jar", jar.toString(), "graceful"), stop);
        var config = new WrapperConfig.Builder().scanRoot(work).minecraftWorkingDirectory(work).minecraftLaunch(spec)
                .tlsEnabled(false).httpPort(0).scanPaths(List.of()).build();
        game = new MinecraftServerProcess(config, new ConsoleBuffer(), Duration.ofSeconds(5));
        var whitelist = new WhitelistService(work);
        var capabilities = new CapabilityEngine(config.capabilities, new com.vesta.sync.perm.PermissionChecker.AllowAll(),
                whitelist::isWhitelisted, new com.vesta.sync.auth.OpsLevelProvider.None());
        server = new VestaHttpServer(0, new ServerState(config), config, new AuthService(config), new LinkCodeService(60), capabilities,
                whitelist, new MinecraftSessionChallengeService(Duration.ofSeconds(30), (name, serverId) -> Optional.empty()),
                new RealtimeTicketService(Duration.ofSeconds(30)), game, null, true);
        server.start();
        assertTrue(game.start());
        await(() -> game.consoleBuffer().snapshot(0, 100).lines.stream().anyMatch(line -> line.text.equals("FAKE_READY")));
    }

    private HttpResponse<String> power(String action) throws Exception {
        var request = HttpRequest.newBuilder(URI.create("http://127.0.0.1:" + server.boundPort()).resolve(VestaSync.API_SERVER_POWER))
                .timeout(Duration.ofSeconds(15)).header("Content-Type", "application/json")
                .POST(HttpRequest.BodyPublishers.ofString("{\"action\":\"" + action + "\"}")).build();
        return HttpClient.newBuilder().connectTimeout(Duration.ofSeconds(3)).build().send(request, HttpResponse.BodyHandlers.ofString());
    }

    private boolean output(String text) {
        return game.consoleBuffer().snapshot(0, 100).lines.stream().anyMatch(line -> line.text.equals(text));
    }

    @AfterEach void cleanup() throws Exception {
        if (server != null) server.stop();
        if (game != null) { game.kill(); await(() -> !game.isRunning()); }
        FakeGameFixtures.deleteTreeWithRetry(root);
    }

    @Test void stopAndRestartAreRefusedWithoutEffectWhenTheDefinitionHasNoGracefulStopButKillStillWorks() throws Exception {
        open(StopMode.UNSUPPORTED);
        for (String action : List.of("stop", "restart")) {
            HttpResponse<String> refused = power(action);
            assertEquals(409, refused.statusCode(), action);
            assertTrue(JsonParser.parseString(refused.body()).getAsJsonObject().get("error").getAsString().contains("graceful stop is not supported"));
            assertTrue(game.isRunning()); assertEquals(MinecraftServerProcess.State.RUNNING, game.state());
            assertFalse(output("COMMAND:stop"));
            assertEquals(1, Files.readAllLines(work.resolve("starts.txt")).size());
        }
        HttpResponse<String> killed = power("kill");
        assertEquals(200, killed.statusCode());
        JsonObject body = JsonParser.parseString(killed.body()).getAsJsonObject();
        assertTrue(body.get("changed").getAsBoolean());
        await(() -> !game.isRunning());
    }

    @Test void stopStillSucceedsThroughTheRouteWhenTheDefinitionDeclaresConsoleStop() throws Exception {
        open(StopMode.CONSOLE_STOP);
        HttpResponse<String> stopped = power("stop");
        assertEquals(200, stopped.statusCode());
        assertTrue(JsonParser.parseString(stopped.body()).getAsJsonObject().get("changed").getAsBoolean());
        await(() -> !game.isRunning());
    }
}
