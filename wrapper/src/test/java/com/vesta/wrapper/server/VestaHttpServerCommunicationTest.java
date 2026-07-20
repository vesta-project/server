package com.vesta.wrapper.server;

import com.google.gson.JsonObject;
import com.google.gson.JsonParser;
import com.vesta.sync.VestaSync;
import com.vesta.sync.auth.CapabilityEngine;
import com.vesta.sync.auth.LinkCodeService;
import com.vesta.sync.auth.MinecraftSessionChallengeService;
import com.vesta.sync.auth.RealtimeTicketService;
import com.vesta.sync.auth.SecurityMode;
import com.vesta.sync.perm.PermissionChecker;
import com.vesta.wrapper.auth.AuthService;
import com.vesta.wrapper.config.SecuritySettingsService;
import com.vesta.wrapper.config.WrapperConfig;
import com.vesta.wrapper.security.WhitelistService;
import org.junit.jupiter.api.AfterEach;
import org.junit.jupiter.api.BeforeEach;
import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.io.TempDir;

import java.net.URI;
import java.net.http.HttpClient;
import java.net.http.HttpRequest;
import java.net.http.HttpResponse;
import java.net.http.WebSocket;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.time.Duration;
import java.util.UUID;
import java.util.concurrent.BlockingQueue;
import java.util.concurrent.CompletableFuture;
import java.util.concurrent.CompletionException;
import java.util.concurrent.CompletionStage;
import java.util.concurrent.LinkedBlockingQueue;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.atomic.AtomicReference;

import static org.junit.jupiter.api.Assertions.*;

class VestaHttpServerCommunicationTest {

    @TempDir
    Path root;

    private final UUID serverUuid = UUID.randomUUID();
    private final UUID accountUuid = UUID.randomUUID();
    private final AtomicReference<String> verifiedServerId = new AtomicReference<>();
    private final HttpClient client = HttpClient.newBuilder()
            .connectTimeout(Duration.ofSeconds(3))
            .build();

    private VestaHttpServer server;
    private AuthService auth;
    private CapabilityEngine capabilities;
    private SecuritySettingsService securitySettings;
    private Path configFile;
    private URI base;

    @BeforeEach
    void startServer() throws Exception {
        Path configDirectory = Files.createDirectories(root.resolve("config"));
        Files.write(configDirectory.resolve("vesta-test.txt"),
                "communication fixture".getBytes(StandardCharsets.UTF_8));
        configFile = root.resolve("wrapper-config.toml");
        Files.writeString(configFile, """
                [security]
                mode = "linked"
                password = ""
                token-ttl-seconds = 86400
                control-port = 9091
                anonymous-level = 0
                authenticated-level = 1
                password-level = 4
                linked-level = 1
                whitelist-level = 3
                """, StandardCharsets.UTF_8);

        WrapperConfig config = new WrapperConfig.Builder()
                .scanRoot(root)
                .scanPaths(java.util.List.of("config"))
                .httpPort(0)
                .tlsEnabled(false)
                .serverId(serverUuid)
                .serverDisplayName("Test Vesta Server")
                .serverDescription("Fixture MOTD")
                .modpackName("Test Pack")
                .modpackVersion("2.4")
                .modpackRevision(12)
                .minimumRevision(9)
                .recommendedMemoryMinMb(4096)
                .recommendedMemoryMaxMb(8192)
                .securityMode(SecurityMode.LINKED)
                .build();
        ServerState state = new ServerState(config);
        auth = new AuthService(config);
        WhitelistService whitelist = new WhitelistService(root);
        capabilities = new CapabilityEngine(
                config.capabilities,
                new PermissionChecker.AllowAll(),
                whitelist::isWhitelisted,
                new com.vesta.sync.auth.OpsLevelProvider.None()
        );
        securitySettings = new SecuritySettingsService(configFile, auth, capabilities, 9091);
        MinecraftSessionChallengeService challenges = new MinecraftSessionChallengeService(
                Duration.ofSeconds(30),
                (name, serverId) -> {
                    verifiedServerId.set(serverId);
                    if (!"PlayerOne".equals(name)) {
                        return java.util.Optional.empty();
                    }
                    return java.util.Optional.of(
                            new MinecraftSessionChallengeService.VerifiedProfile(accountUuid, name));
                }
        );
        server = new VestaHttpServer(
                0,
                state,
                config,
                auth,
                new LinkCodeService(60),
                capabilities,
                whitelist,
                challenges,
                new RealtimeTicketService(Duration.ofSeconds(30)),
                new com.vesta.wrapper.runtime.MinecraftServerProcess(config),
                null,
                true,
                securitySettings
        );
        server.start();
        base = URI.create("http://127.0.0.1:" + server.boundPort());
    }

    @AfterEach
    void stopServer() throws Exception {
        if (server != null) {
            server.stop();
        }
    }

    @Test
    void descriptorIsPublicButManifestAndDownloadsRequireLinkedAccess() throws Exception {
        HttpResponse<String> descriptorResponse = get(VestaSync.API_SERVER_DESCRIPTOR, null);
        assertEquals(200, descriptorResponse.statusCode());
        JsonObject descriptor = json(descriptorResponse);
        assertEquals(serverUuid.toString(), descriptor.get("server_id").getAsString());
        assertEquals(VestaSync.API_VERSION, descriptor.get("protocol_version").getAsString());
        assertEquals("Test Vesta Server", descriptor.get("name").getAsString());
        assertTrue(descriptor.getAsJsonArray("capabilities").contains(
                new com.google.gson.JsonPrimitive("minecraft_challenge")));
        JsonObject pack = descriptor.getAsJsonObject("pack");
        assertEquals(12, pack.get("current_revision").getAsLong());
        assertEquals(9, pack.get("minimum_revision").getAsLong());
        assertEquals(4096, pack.getAsJsonObject("memory").get("minimum_mb").getAsInt());
        assertTrue(pack.get("access_requires_auth").getAsBoolean());

        assertEquals(401, get(VestaSync.API_MANIFEST, null).statusCode());
        String bearer = auth.issueLinkedToken(accountUuid);
        HttpResponse<String> manifestResponse = get(VestaSync.API_MANIFEST, bearer);
        assertEquals(200, manifestResponse.statusCode());
        JsonObject manifest = json(manifestResponse);
        assertEquals(12, manifest.get("revision").getAsLong());
        assertEquals(9, manifest.get("minimum_revision").getAsLong());

        JsonObject file = manifest.getAsJsonArray("overrides").get(0).getAsJsonObject();
        String hash = file.get("sha256").getAsString();
        HttpResponse<byte[]> download = client.send(
                request(VestaSync.API_FILES + hash, bearer).GET().build(),
                HttpResponse.BodyHandlers.ofByteArray());
        assertEquals(200, download.statusCode());
        assertArrayEquals("communication fixture".getBytes(StandardCharsets.UTF_8), download.body());
        assertEquals(401, get(VestaSync.API_FILES + hash, null).statusCode());
    }

    @Test
    void minecraftChallengeIssuesAProfileBoundBearerAndRejectsReplay() throws Exception {
        JsonObject request = new JsonObject();
        request.addProperty("profile_uuid", accountUuid.toString().replace("-", ""));
        request.addProperty("profile_name", "PlayerOne");
        HttpResponse<String> challengeResponse = post(
                VestaSync.API_AUTH_MINECRAFT_CHALLENGE, request, null);
        assertEquals(200, challengeResponse.statusCode());
        JsonObject challenge = json(challengeResponse);
        assertEquals(serverUuid.toString(), challenge.get("vesta_server_id").getAsString());
        assertFalse(challenge.get("server_id").getAsString().isBlank());

        JsonObject complete = new JsonObject();
        complete.addProperty("challenge_id", challenge.get("challenge_id").getAsString());
        HttpResponse<String> completeResponse = post(
                VestaSync.API_AUTH_MINECRAFT_COMPLETE, complete, null);
        assertEquals(200, completeResponse.statusCode());
        JsonObject credential = json(completeResponse);
        assertEquals(accountUuid.toString(), credential.get("uuid").getAsString());
        assertFalse(credential.get("token").getAsString().isBlank());
        assertEquals(challenge.get("server_id").getAsString(), verifiedServerId.get());
        assertEquals(200, get(VestaSync.API_MANIFEST, credential.get("token").getAsString()).statusCode());

        assertEquals(401, post(
                VestaSync.API_AUTH_MINECRAFT_COMPLETE, complete, null).statusCode());
    }

    @Test
    void realtimeRequiresAnAccountTicketAuthenticatesFirstAndConsumesOnce() throws Exception {
        assertEquals(401, post(VestaSync.API_REALTIME_TICKET, new JsonObject(), null).statusCode());
        String bearer = auth.issueLinkedToken(accountUuid);
        JsonObject ticket = json(post(VestaSync.API_REALTIME_TICKET, new JsonObject(), bearer));

        RecordingListener listener = new RecordingListener();
        WebSocket socket = client.newWebSocketBuilder()
                .connectTimeout(Duration.ofSeconds(3))
                .buildAsync(wsUri(), listener)
                .join();
        socket.sendText("{\"type\":\"authenticate\",\"ticket\":\""
                + ticket.get("ticket").getAsString() + "\"}", true).join();
        assertEquals("ready", messageType(listener.nextMessage()));

        socket.sendText("{\"type\":\"ping\"}", true).join();
        assertEquals("pong", messageType(listener.nextMessage()));
        socket.sendClose(WebSocket.NORMAL_CLOSURE, "done").join();

        RecordingListener replay = new RecordingListener();
        WebSocket replaySocket = client.newWebSocketBuilder().buildAsync(wsUri(), replay).join();
        replaySocket.sendText("{\"type\":\"authenticate\",\"ticket\":\""
                + ticket.get("ticket").getAsString() + "\"}", true).join();
        assertEquals(1008, replay.closed.get(3, TimeUnit.SECONDS).intValue());

        RecordingListener unauthenticated = new RecordingListener();
        WebSocket unauthenticatedSocket = client.newWebSocketBuilder()
                .buildAsync(wsUri(), unauthenticated).join();
        unauthenticatedSocket.sendText("{\"type\":\"status\"}", true).join();
        assertEquals(1008, unauthenticated.closed.get(3, TimeUnit.SECONDS).intValue());
    }

    @Test
    void realtimeClosesPromptlyWhenItsBearerIsRevoked() throws Exception {
        String bearer = auth.issueLinkedToken(accountUuid);
        JsonObject ticket = json(post(VestaSync.API_REALTIME_TICKET, new JsonObject(), bearer));
        RecordingListener listener = new RecordingListener();
        WebSocket socket = client.newWebSocketBuilder().buildAsync(wsUri(), listener).join();
        socket.sendText("{\"type\":\"authenticate\",\"ticket\":\""
                + ticket.get("ticket").getAsString() + "\"}", true).join();
        assertEquals("ready", messageType(listener.nextMessage()));

        auth.store().revoke(bearer);
        socket.sendText("{\"type\":\"status\"}", true).join();

        JsonObject changed = JsonParser.parseString(listener.nextMessage()).getAsJsonObject();
        assertEquals("access_changed", changed.get("type").getAsString());
        assertEquals("access_required", changed.get("access_state").getAsString());
        assertEquals(1008, listener.closed.get(3, TimeUnit.SECONDS).intValue());
    }

    @Test
    void realtimeRejectsAnUntrustedBrowserOriginDuringUpgrade() {
        CompletionException error = assertThrows(CompletionException.class, () ->
                client.newWebSocketBuilder()
                        .header("Origin", "https://evil.example")
                        .buildAsync(wsUri(), new RecordingListener())
                        .join());

        assertTrue(error.getCause() instanceof java.net.http.WebSocketHandshakeException);
        java.net.http.WebSocketHandshakeException handshake =
                (java.net.http.WebSocketHandshakeException) error.getCause();
        assertEquals(403, handshake.getResponse().statusCode());
    }

    @Test
    void adminSecurityIsLoopbackReadableAndUpdatesLiveAccess() throws Exception {
        HttpResponse<String> get = get(VestaSync.API_ADMIN_SECURITY, null);
        assertEquals(200, get.statusCode());
        JsonObject security = json(get);
        assertEquals("linked", security.get("mode").getAsString());
        assertFalse(security.get("password_configured").getAsBoolean());
        assertFalse(security.has("password"));

        UUID player = UUID.randomUUID();
        JsonObject body = new JsonObject();
        body.addProperty("mode", "open");
        body.addProperty("anonymous_level", 1);
        com.google.gson.JsonArray people = new com.google.gson.JsonArray();
        JsonObject person = new JsonObject();
        person.addProperty("uuid", player.toString());
        person.addProperty("level", 4);
        people.add(person);
        body.add("people", people);

        HttpResponse<String> put = put(VestaSync.API_ADMIN_SECURITY, body, null);
        assertEquals(200, put.statusCode());
        JsonObject updated = json(put);
        assertEquals("open", updated.get("mode").getAsString());
        assertEquals(SecurityMode.OPEN, auth.mode());
        assertEquals(1, capabilities.config().anonymousBaseline);
        assertEquals(4, capabilities.config().people.get(player).intValue());
        assertFalse(updated.get("access_requires_auth").getAsBoolean());

        HttpResponse<String> status = get(VestaSync.API_STATUS, null);
        assertEquals(200, status.statusCode());
        assertEquals("open", json(status).get("security_mode").getAsString());
        assertFalse(json(status).get("access_requires_auth").getAsBoolean());
    }

    @Test
    void adminSecurityRejectsPasswordModeWithoutPassword() throws Exception {
        JsonObject body = new JsonObject();
        body.addProperty("mode", "password");
        body.addProperty("clear_password", true);
        HttpResponse<String> put = put(VestaSync.API_ADMIN_SECURITY, body, null);
        assertEquals(400, put.statusCode());
        assertTrue(json(put).get("error").getAsString().toLowerCase().contains("password"));
    }

    @Test
    void statusExposesMinecraftLifecycleFields() throws Exception {
        HttpResponse<String> status = get(VestaSync.API_STATUS, null);
        assertEquals(200, status.statusCode());
        JsonObject body = json(status);
        assertTrue(body.has("minecraft_manageable"));
        assertTrue(body.has("minecraft_running"));
        assertTrue(body.has("minecraft"));
        assertEquals("stopped", body.getAsJsonObject("minecraft").get("state").getAsString());
    }

    @Test
    void serverPowerRejectsUnknownActionAndMissingLaunchJar() throws Exception {
        JsonObject bad = new JsonObject();
        bad.addProperty("action", "pause");
        assertEquals(400, post(VestaSync.API_SERVER_POWER, bad, null).statusCode());

        JsonObject stop = new JsonObject();
        stop.addProperty("action", "stop");
        HttpResponse<String> response = post(VestaSync.API_SERVER_POWER, stop, null);
        assertEquals(409, response.statusCode());
        assertTrue(json(response).get("error").getAsString().contains("launch-jar"));
    }

    private HttpResponse<String> get(String path, String bearer) throws Exception {
        return client.send(request(path, bearer).GET().build(), HttpResponse.BodyHandlers.ofString());
    }

    private HttpResponse<String> post(String path, JsonObject body, String bearer) throws Exception {
        return client.send(
                request(path, bearer)
                        .header("Content-Type", "application/json")
                        .POST(HttpRequest.BodyPublishers.ofString(body.toString()))
                        .build(),
                HttpResponse.BodyHandlers.ofString());
    }

    private HttpResponse<String> put(String path, JsonObject body, String bearer) throws Exception {
        return client.send(
                request(path, bearer)
                        .header("Content-Type", "application/json")
                        .PUT(HttpRequest.BodyPublishers.ofString(body.toString()))
                        .build(),
                HttpResponse.BodyHandlers.ofString());
    }

    private HttpRequest.Builder request(String path, String bearer) {
        HttpRequest.Builder builder = HttpRequest.newBuilder(base.resolve(path))
                .timeout(Duration.ofSeconds(3));
        if (bearer != null) {
            builder.header("Authorization", "Bearer " + bearer);
        }
        return builder;
    }

    private URI wsUri() {
        return URI.create("ws://127.0.0.1:" + server.boundPort() + VestaSync.API_REALTIME);
    }

    private static JsonObject json(HttpResponse<String> response) {
        return JsonParser.parseString(response.body()).getAsJsonObject();
    }

    private static String messageType(String message) {
        return JsonParser.parseString(message).getAsJsonObject().get("type").getAsString();
    }

    private static final class RecordingListener implements WebSocket.Listener {
        private final BlockingQueue<String> messages = new LinkedBlockingQueue<>();
        private final CompletableFuture<Integer> closed = new CompletableFuture<>();
        private final StringBuilder partial = new StringBuilder();

        @Override
        public void onOpen(WebSocket webSocket) {
            webSocket.request(1);
        }

        @Override
        public CompletionStage<?> onText(WebSocket webSocket, CharSequence data, boolean last) {
            partial.append(data);
            if (last) {
                messages.add(partial.toString());
                partial.setLength(0);
            }
            webSocket.request(1);
            return CompletableFuture.completedFuture(null);
        }

        @Override
        public CompletionStage<?> onClose(WebSocket webSocket, int statusCode, String reason) {
            closed.complete(statusCode);
            return CompletableFuture.completedFuture(null);
        }

        @Override
        public void onError(WebSocket webSocket, Throwable error) {
            closed.completeExceptionally(error);
        }

        String nextMessage() throws InterruptedException {
            String value = messages.poll(3, TimeUnit.SECONDS);
            assertNotNull(value, "timed out waiting for realtime message");
            return value;
        }
    }
}
