package com.vesta.wrapper.server;

import com.google.gson.JsonParser;
import com.vesta.sync.VestaSync;
import com.vesta.sync.auth.CapabilityEngine;
import com.vesta.sync.auth.LinkCodeService;
import com.vesta.sync.auth.MinecraftSessionChallengeService;
import com.vesta.sync.auth.RealtimeTicketService;
import com.vesta.wrapper.auth.AuthService;
import com.vesta.wrapper.config.WrapperConfig;
import com.vesta.wrapper.runtime.MinecraftServerProcess;
import com.vesta.wrapper.security.WhitelistService;
import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.io.TempDir;

import java.net.URI;
import java.net.http.HttpClient;
import java.net.http.HttpRequest;
import java.net.http.HttpResponse;
import java.nio.file.Path;
import java.time.Duration;
import java.util.List;
import java.util.Optional;

import static org.junit.jupiter.api.Assertions.*;

class DurableLifecycleRouteTest {
    @TempDir Path root;

    @Test void legacyPowerAndConsoleMutationAreDisabledEvenOnLoopback() throws Exception {
        var config = new WrapperConfig.Builder().scanRoot(root).minecraftWorkingDirectory(root)
                .tlsEnabled(false).httpPort(0).durableRuntimeEnabled(true).stateDirectory(root.resolveSibling("unused-state"))
                .scanPaths(List.of()).build();
        var auth = new AuthService(config);
        var whitelist = new WhitelistService(root);
        var capabilities = new CapabilityEngine(config.capabilities, new com.vesta.sync.perm.PermissionChecker.AllowAll(),
                whitelist::isWhitelisted, new com.vesta.sync.auth.OpsLevelProvider.None());
        var game = new MinecraftServerProcess(config);
        var server = new VestaHttpServer(0, new ServerState(config), config, auth, new LinkCodeService(60), capabilities,
                whitelist, new MinecraftSessionChallengeService(Duration.ofSeconds(30), (name, serverId) -> Optional.empty()),
                new RealtimeTicketService(Duration.ofSeconds(30)), game, null, true, null, true);
        server.start();
        try {
            var client = HttpClient.newBuilder().connectTimeout(Duration.ofSeconds(3)).build();
            URI base = URI.create("http://127.0.0.1:" + server.boundPort());
            for (String path : List.of(VestaSync.API_SERVER_POWER, VestaSync.API_CONSOLE)) {
                for (String method : List.of("POST", "PUT", "DELETE")) {
                    var request = HttpRequest.newBuilder(base.resolve(path)).timeout(Duration.ofSeconds(5))
                            .header("Content-Type", "application/json")
                            .method(method, HttpRequest.BodyPublishers.ofString("{\"action\":\"kill\",\"command\":\"stop\"}")).build();
                    var response = client.send(request, HttpResponse.BodyHandlers.ofString());
                    assertEquals(409, response.statusCode(), method + " " + path);
                    assertEquals("legacy_lifecycle_disabled", JsonParser.parseString(response.body()).getAsJsonObject().get("error").getAsString());
                    assertFalse(game.isRunning());
                }
            }
            var noNewRoute = client.send(HttpRequest.newBuilder(base.resolve("/api/v2/operations")).timeout(Duration.ofSeconds(5)).GET().build(), HttpResponse.BodyHandlers.ofString());
            assertEquals(404, noNewRoute.statusCode());
        } finally { server.stop(); }
    }
}
