package com.vesta.wrapper.server;

import com.google.gson.FieldNamingPolicy;
import com.google.gson.Gson;
import com.google.gson.GsonBuilder;
import com.google.gson.JsonObject;
import com.vesta.sync.VestaSync;
import com.vesta.sync.auth.AuthGate;
import com.vesta.sync.auth.BearerTokenStore;
import com.vesta.sync.auth.AccessCapability;
import com.vesta.sync.auth.CapabilityEngine;
import com.vesta.sync.auth.LinkCodeService;
import com.vesta.sync.auth.MinecraftSessionChallengeService;
import com.vesta.sync.auth.RealtimeTicketService;
import com.vesta.sync.manifest.LocalArchivePack;
import com.vesta.sync.manifest.ManifestFile;
import com.vesta.sync.manifest.ModpackMode;
import com.vesta.sync.manifest.ServerModpackManifest;
import com.vesta.sync.sync.SyncCheckResult;
import com.vesta.wrapper.auth.AuthService;
import com.vesta.wrapper.config.SecuritySettingsService;
import com.vesta.wrapper.config.WrapperConfig;
import com.vesta.wrapper.content.ContentInstaller;
import com.vesta.wrapper.ipc.ControlIpcServer;
import com.vesta.wrapper.runtime.MinecraftServerProcess;
import com.vesta.wrapper.security.WhitelistService;
import org.eclipse.jetty.http.HttpHeader;
import org.eclipse.jetty.http.HttpMethod;
import org.eclipse.jetty.http.HttpVersion;
import org.eclipse.jetty.io.Content;
import org.eclipse.jetty.server.Handler;
import org.eclipse.jetty.server.HttpConfiguration;
import org.eclipse.jetty.server.HttpConnectionFactory;
import org.eclipse.jetty.server.Request;
import org.eclipse.jetty.server.Response;
import org.eclipse.jetty.server.Server;
import org.eclipse.jetty.server.ServerConnector;
import org.eclipse.jetty.server.SecureRequestCustomizer;
import org.eclipse.jetty.server.SslConnectionFactory;
import org.eclipse.jetty.util.Callback;
import org.eclipse.jetty.util.ssl.SslContextFactory;
import org.eclipse.jetty.websocket.server.ServerWebSocketContainer;
import org.eclipse.jetty.websocket.server.WebSocketUpgradeHandler;
import org.slf4j.Logger;
import org.slf4j.LoggerFactory;

import java.net.InetAddress;
import java.net.InetSocketAddress;
import java.net.SocketAddress;
import java.io.ByteArrayInputStream;
import java.io.InputStream;
import java.net.URLDecoder;
import java.nio.ByteBuffer;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.ArrayList;
import java.util.Base64;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Locale;
import java.util.Map;
import java.util.Optional;
import java.util.Set;
import java.util.UUID;
import java.time.Duration;

public final class VestaHttpServer {

    private static final Logger log = LoggerFactory.getLogger(VestaHttpServer.class);
    private static final Gson GSON = new GsonBuilder()
            .setFieldNamingPolicy(FieldNamingPolicy.LOWER_CASE_WITH_UNDERSCORES)
            .setPrettyPrinting()
            .create();
    private final Server jetty;
    private final ServerState serverState;
    private final WrapperConfig config;
    private final AuthService auth;
    private final LinkCodeService linkCodes;
    private final CapabilityEngine capabilities;
    private final WhitelistService whitelist;
    private final MinecraftSessionChallengeService minecraftChallenges;
    private final RealtimeTicketService realtimeTickets;
    private final MinecraftServerProcess minecraft;
    private final ControlIpcServer controlIpc;
    private final SecuritySettingsService securitySettings;

    public VestaHttpServer(
            int port,
            ServerState serverState,
            WrapperConfig config,
            AuthService auth,
            LinkCodeService linkCodes,
            CapabilityEngine capabilities,
            WhitelistService whitelist,
            MinecraftSessionChallengeService minecraftChallenges,
            RealtimeTicketService realtimeTickets,
            MinecraftServerProcess minecraft,
            ControlIpcServer controlIpc,
            boolean bindLocalOnly
    ) {
        this(port, serverState, config, auth, linkCodes, capabilities, whitelist,
                minecraftChallenges, realtimeTickets, minecraft, controlIpc, bindLocalOnly, null);
    }

    public VestaHttpServer(
            int port,
            ServerState serverState,
            WrapperConfig config,
            AuthService auth,
            LinkCodeService linkCodes,
            CapabilityEngine capabilities,
            WhitelistService whitelist,
            MinecraftSessionChallengeService minecraftChallenges,
            RealtimeTicketService realtimeTickets,
            MinecraftServerProcess minecraft,
            ControlIpcServer controlIpc,
            boolean bindLocalOnly,
            SecuritySettingsService securitySettings
    ) {
        this.serverState = serverState;
        this.config = config;
        this.auth = auth;
        this.linkCodes = linkCodes;
        this.capabilities = capabilities;
        this.whitelist = whitelist;
        this.minecraftChallenges = minecraftChallenges;
        this.realtimeTickets = realtimeTickets;
        this.minecraft = minecraft;
        this.controlIpc = controlIpc;
        this.securitySettings = securitySettings;
        this.jetty = new Server();
        ServerConnector connector;
        if (config.tlsEnabled) {
            SslContextFactory.Server tls = new SslContextFactory.Server();
            tls.setKeyStorePath(config.tlsKeyStore.toAbsolutePath().normalize().toString());
            tls.setKeyStorePassword(config.tlsKeyStorePassword);
            HttpConfiguration https = new HttpConfiguration();
            https.addCustomizer(new SecureRequestCustomizer());
            connector = new ServerConnector(
                    jetty,
                    new SslConnectionFactory(tls, HttpVersion.HTTP_1_1.asString()),
                    new HttpConnectionFactory(https)
            );
        } else {
            connector = new ServerConnector(jetty);
        }
        connector.setPort(port);
        if (bindLocalOnly || !config.tlsEnabled) {
            connector.setHost("127.0.0.1");
        }
        jetty.addConnector(connector);
        Handler httpHandler = new Handler.Abstract() {
            @Override
            public boolean handle(Request request, Response response, Callback callback) throws Exception {
                handleRequest(request, response, callback);
                return true;
            }
        };
        WebSocketUpgradeHandler realtimeHandler = WebSocketUpgradeHandler.from(jetty, container -> {
            configureRealtime(container);
            container.addMapping(VestaSync.API_REALTIME, (request, response, callback) -> {
                if (!isAllowedRealtimeOrigin(request.getHeaders().get(HttpHeader.ORIGIN))) {
                    response.setStatus(403);
                    callback.succeeded();
                    return null;
                }
                return new VestaRealtimeSocket(
                        config.serverId,
                        realtimeTickets,
                        serverState,
                        auth,
                        capabilities
                );
            });
        });
        realtimeHandler.setHandler(httpHandler);
        jetty.setHandler(realtimeHandler);
    }

    public void start() throws Exception {
        jetty.start();
        log.info("{} REST API listening on port {}",
                config.tlsEnabled ? "HTTPS" : "HTTP",
                ((ServerConnector) jetty.getConnectors()[0]).getPort());
    }

    public void stop() throws Exception {
        jetty.stop();
        log.info("HTTP REST API stopped");
    }

    /** Returns the active listener port, including an ephemeral port selected from port 0. */
    public int boundPort() {
        return ((ServerConnector) jetty.getConnectors()[0]).getLocalPort();
    }

    private void handleRequest(Request request, Response response, Callback callback) throws Exception {
        String path = request.getHttpURI().getPath();
        applyCors(request, response);

        try {
            if (HttpMethod.OPTIONS.is(request.getMethod())) {
                response.setStatus(204);
                response.write(true, ByteBuffer.allocate(0), callback);
                return;
            }
            if ("/devtools".equals(path) || "/devtools/".equals(path) || "/".equals(path)) {
                serveDevtools(response, callback);
                return;
            }
            switch (path) {
                case VestaSync.API_SERVER_DESCRIPTOR -> {
                    response.setStatus(200);
                    handleServerDescriptor(request, response, callback);
                    return;
                }
                case VestaSync.API_MODPACK_INFO -> {
                    if (!requireCapability(request, response, callback, CapabilityEngine.Action.PUBLIC_MODPACK)) return;
                    response.setStatus(200);
                    handleModpackInfo(response, callback);
                    return;
                }
                case VestaSync.API_MANIFEST -> {
                    if (!requireCapability(request, response, callback, CapabilityEngine.Action.PUBLIC_MODPACK)) return;
                    response.setStatus(200);
                    handleModpackManifest(response, callback);
                    return;
                }
                case VestaSync.API_MODPACK_ARCHIVE -> {
                    if (!requireCapability(request, response, callback, CapabilityEngine.Action.PUBLIC_MODPACK)) return;
                    handleModpackArchive(response, callback);
                    return;
                }
                case VestaSync.API_MODPACK_VERSIONS -> {
                    if (!requireCapability(request, response, callback, CapabilityEngine.Action.PUBLIC_MODPACK)) return;
                    response.setStatus(200);
                    handleModpackVersions(response, callback);
                    return;
                }
                case VestaSync.API_SYNC_CHECK -> {
                    if (!requireCapability(request, response, callback, CapabilityEngine.Action.PUBLIC_MODPACK)) return;
                    handleSyncCheck(request, response, callback);
                    return;
                }
                case VestaSync.API_AUTH_TOKEN -> {
                    handleAuthToken(request, response, callback);
                    return;
                }
                case VestaSync.API_AUTH_LINK -> {
                    handleAuthLink(request, response, callback);
                    return;
                }
                case VestaSync.API_AUTH_MINECRAFT_CHALLENGE -> {
                    handleMinecraftChallenge(request, response, callback);
                    return;
                }
                case VestaSync.API_AUTH_MINECRAFT_COMPLETE -> {
                    handleMinecraftComplete(request, response, callback);
                    return;
                }
                case VestaSync.API_REALTIME_TICKET -> {
                    handleRealtimeTicket(request, response, callback);
                    return;
                }
                case VestaSync.API_FILES_UPLOAD -> {
                    if (!requireCapability(request, response, callback, CapabilityEngine.Action.MANAGE_CONTENT)) return;
                    handleUpload(request, response, callback);
                    return;
                }
                case VestaSync.API_CONTENT_REMOVE -> {
                    if (!requireCapability(request, response, callback, CapabilityEngine.Action.MANAGE_CONTENT)) return;
                    handleRemove(request, response, callback);
                    return;
                }
                case VestaSync.API_STATUS -> {
                    response.setStatus(200);
                    handleStatus(response, callback);
                    return;
                }
                case VestaSync.API_SERVER_POWER -> {
                    if (!allowLocalAdmin(request) && !requireCapability(
                            request, response, callback, AccessCapability.SERVER_POWER)) {
                        return;
                    }
                    handleServerPower(request, response, callback);
                    return;
                }
                case VestaSync.API_ADMIN_SECURITY -> {
                    if (!requireLoopback(request, response, callback)) {
                        return;
                    }
                    handleAdminSecurity(request, response, callback);
                    return;
                }
                case VestaSync.API_PRIVATE_FILES -> {
                    response.setStatus(410);
                    json(response, callback, Map.of(
                            "error", "deprecated",
                            "message", "Private file listing was merged into GET /api/v1/modpack/manifest (pack.fetch)."
                    ));
                    return;
                }
                default -> {
                    if (path.startsWith(VestaSync.API_FILES)) {
                        if (!requireCapability(request, response, callback, CapabilityEngine.Action.PUBLIC_MODPACK)) return;
                        handleFileDownload(request, response, callback);
                        return;
                    }
                    if (path.startsWith(VestaSync.API_PRIVATE_FILES + "/")) {
                        response.setStatus(410);
                        json(response, callback, Map.of(
                                "error", "deprecated",
                                "message", "Private file downloads use GET /api/v1/files/{sha256} with pack.fetch."
                        ));
                        return;
                    }
                    if (path.startsWith(VestaSync.API_MODPACK_VERSIONS + "/") && path.endsWith("/manifest")) {
                        if (!requireCapability(request, response, callback, CapabilityEngine.Action.PUBLIC_MODPACK)) return;
                        handleVersionManifest(request, response, callback);
                        return;
                    }
                }
            }
            response.setStatus(404);
            json(response, callback, Map.of("error", "not found"));
        } catch (Exception e) {
            log.error("HTTP error: {}", e.getMessage(), e);
            response.setStatus(500);
            json(response, callback, Map.of("error", "internal server error"));
        }
    }

    private CapabilityEngine.AuthContext authContext(Request request) {
        String header = request.getHeaders().get(VestaSync.AUTH_HEADER);
        Optional<BearerTokenStore.Record> rec = auth.validateBearer(header);
        boolean authenticated = auth.isOpen() || rec.isPresent();
        boolean passwordAuth = rec.isPresent() && "operator".equals(rec.get().role);
        UUID uuid = rec.map(r -> r.boundUuid).orElse(null);
        String role = rec.map(r -> r.role).orElse(null);
        return new CapabilityEngine.AuthContext(auth.mode(), authenticated, passwordAuth, uuid, role);
    }

    private boolean requireCapability(
            Request request,
            Response response,
            Callback callback,
            CapabilityEngine.Action action
    ) {
        return requireCapability(request, response, callback, capabilities.check(action, authContext(request)));
    }

    private boolean requireCapability(
            Request request,
            Response response,
            Callback callback,
            String capability
    ) {
        return requireCapability(request, response, callback, capabilities.check(capability, authContext(request)));
    }

    private boolean requireCapability(
            Request request,
            Response response,
            Callback callback,
            CapabilityEngine.Decision d
    ) {
        if (d == CapabilityEngine.Decision.ALLOW) {
            return true;
        }
        switch (d) {
            case DENY_AUTH:
                response.setStatus(401);
                json(response, callback, Map.of("error", "authentication required"));
                return false;
            case DENY_WHITELIST:
                response.setStatus(403);
                json(response, callback, Map.of("error", "not whitelisted"));
                return false;
            case DENY_PERM_UNAVAILABLE:
                response.setStatus(403);
                json(response, callback, Map.of("error", "mod offline — permission check unavailable"));
                return false;
            default:
                response.setStatus(403);
                json(response, callback, Map.of("error", "permission denied"));
                return false;
        }
    }

    private void handleAuthToken(Request request, Response response, Callback callback) throws Exception {
        if (!HttpMethod.POST.is(request.getMethod())) {
            response.setStatus(405);
            json(response, callback, Map.of("error", "method not allowed"));
            return;
        }
        if (auth.isOpen()) {
            String token = auth.store().issue("anonymous", null);
            response.setStatus(200);
            json(response, callback, Map.of("token", token, "mode", "open"));
            return;
        }
        JsonObject body = GSON.fromJson(readBody(request), JsonObject.class);
        String password = body != null && body.has("password") ? body.get("password").getAsString() : "";
        Optional<String> token = auth.login(password);
        if (token.isEmpty()) {
            response.setStatus(401);
            json(response, callback, Map.of("error", "invalid password"));
            return;
        }
        response.setStatus(200);
        json(response, callback, Map.of("token", token.get(), "token_type", "Bearer"));
    }

    private void handleRealtimeTicket(
            Request request,
            Response response,
            Callback callback
    ) {
        if (!HttpMethod.POST.is(request.getMethod())) {
            response.setStatus(405);
            json(response, callback, Map.of("error", "method not allowed"));
            return;
        }
        if (!requireCapability(
                request,
                response,
                callback,
                CapabilityEngine.Action.PUBLIC_MODPACK
        )) {
            return;
        }
        Optional<BearerTokenStore.Record> bearer = auth.validateBearer(
                request.getHeaders().get(VestaSync.AUTH_HEADER)
        );
        if (bearer.isEmpty()) {
            response.setStatus(401);
            json(response, callback, Map.of("error", "authentication required"));
            return;
        }
        if (bearer.get().boundUuid == null) {
            response.setStatus(403);
            json(response, callback, Map.of(
                    "error", "realtime requires an account-linked credential"
            ));
            return;
        }
        RealtimeTicketService.Issued issued = realtimeTickets.issue(
                config.serverId,
                bearer.get(),
                Set.of("server_status", "pack_updates", "access_changes")
        );
        Map<String, Object> body = new LinkedHashMap<>();
        body.put("ticket", issued.value);
        body.put("expires_at", issued.expiresAt.toString());
        response.setStatus(200);
        json(response, callback, body);
    }

    private void handleMinecraftChallenge(
            Request request,
            Response response,
            Callback callback
    ) throws Exception {
        if (!HttpMethod.POST.is(request.getMethod())) {
            response.setStatus(405);
            json(response, callback, Map.of("error", "method not allowed"));
            return;
        }
        JsonObject body = GSON.fromJson(readBody(request), JsonObject.class);
        String rawUuid = body != null && body.has("profile_uuid")
                ? body.get("profile_uuid").getAsString()
                : "";
        String profileName = body != null && body.has("profile_name")
                ? body.get("profile_name").getAsString()
                : "";
        try {
            UUID profileUuid = parseUuid(rawUuid);
            var issued = minecraftChallenges.issue(
                    config.serverId,
                    profileUuid,
                    profileName
            );
            Map<String, Object> out = new LinkedHashMap<>();
            out.put("challenge_id", issued.challengeId());
            out.put("server_id", issued.serverId());
            out.put("expires_at", issued.expiresAt().toString());
            out.put("vesta_server_id", config.serverId.toString());
            response.setStatus(200);
            json(response, callback, out);
        } catch (IllegalArgumentException e) {
            response.setStatus(400);
            json(response, callback, Map.of("error", e.getMessage()));
        }
    }

    private void handleMinecraftComplete(
            Request request,
            Response response,
            Callback callback
    ) throws Exception {
        if (!HttpMethod.POST.is(request.getMethod())) {
            response.setStatus(405);
            json(response, callback, Map.of("error", "method not allowed"));
            return;
        }
        JsonObject body = GSON.fromJson(readBody(request), JsonObject.class);
        String challengeId = body != null && body.has("challenge_id")
                ? body.get("challenge_id").getAsString()
                : "";
        Optional<MinecraftSessionChallengeService.VerifiedProfile> verified =
                minecraftChallenges.complete(config.serverId, challengeId);
        if (verified.isEmpty()) {
            response.setStatus(401);
            json(response, callback, Map.of("error", "invalid, expired, or unverified challenge"));
            return;
        }

        var profile = verified.get();
        CapabilityEngine.AuthContext context = new CapabilityEngine.AuthContext(
                auth.mode(),
                true,
                false,
                profile.uuid(),
                "linked"
        );
        CapabilityEngine.Decision decision = capabilities.check(
                CapabilityEngine.Action.PUBLIC_MODPACK,
                context
        );
        if (decision != CapabilityEngine.Decision.ALLOW) {
            response.setStatus(403);
            json(response, callback, Map.of("error", "profile does not have pack access"));
            return;
        }

        String token = auth.issueMinecraftToken(profile.uuid(), profile.name());
        Map<String, Object> out = new LinkedHashMap<>();
        out.put("token", token);
        out.put("token_type", "Bearer");
        out.put("uuid", profile.uuid().toString());
        out.put("name", profile.name());
        response.setStatus(200);
        json(response, callback, out);
    }

    private void handleAuthLink(Request request, Response response, Callback callback) throws Exception {
        if (!HttpMethod.POST.is(request.getMethod())) {
            response.setStatus(405);
            json(response, callback, Map.of("error", "method not allowed"));
            return;
        }
        JsonObject body = GSON.fromJson(readBody(request), JsonObject.class);
        String code = body != null && body.has("code") ? body.get("code").getAsString() : "";
        Optional<UUID> uuid = linkCodes.redeem(code);
        if (uuid.isEmpty()) {
            response.setStatus(400);
            json(response, callback, Map.of("error", "invalid or expired link code"));
            return;
        }
        String token = auth.issueLinkedToken(uuid.get());
        Map<String, Object> out = new LinkedHashMap<>();
        out.put("token", token);
        out.put("token_type", "Bearer");
        out.put("uuid", uuid.get().toString());
        response.setStatus(200);
        json(response, callback, out);
    }

    private void handleUpload(Request request, Response response, Callback callback) throws Exception {
        if (!HttpMethod.POST.is(request.getMethod())) {
            response.setStatus(405);
            json(response, callback, Map.of("error", "method not allowed"));
            return;
        }
        JsonObject body = GSON.fromJson(readBody(request), JsonObject.class);
        String path = body != null && body.has("path") ? body.get("path").getAsString() : null;
        String b64 = body != null && body.has("content_base64") ? body.get("content_base64").getAsString() : null;
        if (path == null || b64 == null) {
            response.setStatus(400);
            json(response, callback, Map.of("error", "path and content_base64 required"));
            return;
        }
        byte[] data = Base64.getDecoder().decode(b64);
        ContentInstaller.Result result = serverState.install(path, new ByteArrayInputStream(data));
        Map<String, Object> out = new LinkedHashMap<>();
        out.put("path", result.path);
        out.put("sha256", result.sha256);
        out.put("size", result.size);
        out.put("version", serverState.getCurrentVersion());
        response.setStatus(200);
        json(response, callback, out);
    }

    private void handleRemove(Request request, Response response, Callback callback) throws Exception {
        if (!HttpMethod.POST.is(request.getMethod()) && !HttpMethod.DELETE.is(request.getMethod())) {
            response.setStatus(405);
            json(response, callback, Map.of("error", "method not allowed"));
            return;
        }
        JsonObject body = GSON.fromJson(readBody(request), JsonObject.class);
        String path = body != null && body.has("path") ? body.get("path").getAsString() : null;
        if (path == null || path.isBlank()) {
            response.setStatus(400);
            json(response, callback, Map.of("error", "path required"));
            return;
        }
        boolean deleted = serverState.remove(path);
        response.setStatus(200);
        json(response, callback, Map.of("removed", deleted, "version", serverState.getCurrentVersion()));
    }

    private void handleStatus(Response response, Callback callback) {
        ServerModpackManifest manifest = serverState.getManifest();
        Map<String, Object> status = new LinkedHashMap<>();
        status.put("security_mode", auth.mode().name().toLowerCase());
        status.put("modpack_version", serverState.getCurrentVersion());
        status.put("updated_at", serverState.getUpdatedAt().toString());
        status.put("pack_file_count", manifest.mods.size() + manifest.overrides.size());
        status.put("whitelist_count", whitelist.all().size());
        status.put("access_requires_auth", capabilities.config().accessRequiresAuth);
        status.put("password_configured", auth.hasPassword());
        status.put("minecraft_manageable", minecraft != null && minecraft.isConfigured());
        status.put("minecraft_running", minecraft != null && minecraft.isRunning());
        status.put("connector_connected", controlIpc != null && controlIpc.isModConnected());
        if (minecraft != null) {
            status.put("minecraft", minecraft.snapshot());
        }
        json(response, callback, status);
    }

    private void handleServerPower(Request request, Response response, Callback callback) throws Exception {
        if (!HttpMethod.POST.is(request.getMethod())) {
            response.setStatus(405);
            json(response, callback, Map.of("error", "method not allowed"));
            return;
        }
        JsonObject body = GSON.fromJson(readBody(request), JsonObject.class);
        String action = body != null && body.has("action")
                ? body.get("action").getAsString().trim().toLowerCase(Locale.ROOT)
                : "";
        if (!Set.of("start", "stop", "restart", "kill").contains(action)) {
            response.setStatus(400);
            json(response, callback, Map.of("error", "action must be one of: start, stop, restart, kill"));
            return;
        }
        if (minecraft == null || !minecraft.isConfigured()) {
            response.setStatus(409);
            json(response, callback, Map.of("error", "no managed Minecraft server is configured (set minecraft.launch-jar)"));
            return;
        }
        try {
            boolean changed;
            switch (action) {
                case "start" -> changed = minecraft.start();
                case "restart" -> changed = minecraft.restart();
                case "stop" -> {
                    boolean wasRunning = minecraft.isRunning();
                    minecraft.stop();
                    changed = wasRunning;
                }
                case "kill" -> {
                    boolean wasRunning = minecraft.isRunning();
                    minecraft.kill();
                    changed = wasRunning;
                }
                default -> {
                    response.setStatus(400);
                    json(response, callback, Map.of("error", "action must be one of: start, stop, restart, kill"));
                    return;
                }
            }
            Map<String, Object> out = new LinkedHashMap<>();
            out.put("action", action);
            out.put("changed", changed);
            out.put("running", minecraft.isRunning());
            out.put("minecraft", minecraft.snapshot());
            response.setStatus(200);
            json(response, callback, out);
        } catch (java.io.IOException e) {
            response.setStatus(409);
            json(response, callback, Map.of("error", e.getMessage()));
        }
    }

    private void handleAdminSecurity(Request request, Response response, Callback callback) throws Exception {
        if (securitySettings == null) {
            response.setStatus(503);
            json(response, callback, Map.of("error", "security settings service unavailable"));
            return;
        }
        String method = request.getMethod();
        if (HttpMethod.GET.is(method)) {
            response.setStatus(200);
            json(response, callback, securitySettings.snapshot());
            return;
        }
        if (HttpMethod.PUT.is(method) || HttpMethod.POST.is(method)) {
            @SuppressWarnings("unchecked")
            Map<String, Object> body = GSON.fromJson(readBody(request), Map.class);
            if (body == null) {
                body = Map.of();
            }
            try {
                Map<String, Object> snapshot = securitySettings.update(body);
                response.setStatus(200);
                json(response, callback, snapshot);
            } catch (IllegalArgumentException e) {
                response.setStatus(400);
                json(response, callback, Map.of("error", e.getMessage()));
            }
            return;
        }
        if (HttpMethod.DELETE.is(method)) {
            // DELETE /api/v1/admin/security?uuid=... removes a person grant
            String query = request.getHttpURI().getQuery();
            String uuidRaw = queryParam(query, "uuid");
            if (uuidRaw == null || uuidRaw.isBlank()) {
                response.setStatus(400);
                json(response, callback, Map.of("error", "uuid query parameter required"));
                return;
            }
            try {
                UUID uuid = com.vesta.wrapper.config.SecuritySettingsService.parseFlexibleUuid(uuidRaw);
                Map<String, Object> snapshot = securitySettings.upsertPerson(uuid, null);
                response.setStatus(200);
                json(response, callback, snapshot);
            } catch (IllegalArgumentException e) {
                response.setStatus(400);
                json(response, callback, Map.of("error", e.getMessage()));
            }
            return;
        }
        response.setStatus(405);
        json(response, callback, Map.of("error", "method not allowed"));
    }

    private static String queryParam(String query, String name) {
        if (query == null || query.isBlank()) {
            return null;
        }
        for (String part : query.split("&")) {
            int eq = part.indexOf('=');
            String key = eq >= 0 ? part.substring(0, eq) : part;
            String value = eq >= 0 ? part.substring(eq + 1) : "";
            if (name.equals(URLDecoder.decode(key, StandardCharsets.UTF_8))) {
                return URLDecoder.decode(value, StandardCharsets.UTF_8);
            }
        }
        return null;
    }

    private boolean requireLoopback(Request request, Response response, Callback callback) {
        if (allowLocalAdmin(request)) {
            return true;
        }
        response.setStatus(403);
        json(response, callback, Map.of("error", "admin endpoints are localhost-only"));
        return false;
    }

    private boolean allowLocalAdmin(Request request) {
        try {
            SocketAddress remote = request.getConnectionMetaData().getRemoteSocketAddress();
            if (!(remote instanceof InetSocketAddress inet)) {
                return false;
            }
            InetAddress address = inet.getAddress();
            return address != null && address.isLoopbackAddress();
        } catch (Exception e) {
            return false;
        }
    }

    private void handleServerDescriptor(Request request, Response response, Callback callback) {
        ServerModpackManifest manifest = serverState.getManifest();
        Map<String, Object> minecraft = new LinkedHashMap<>();
        minecraft.put("host", config.serverHost);
        minecraft.put("port", config.serverPort);
        if (config.serverDescription != null && !config.serverDescription.isBlank()) {
            minecraft.put("description", config.serverDescription.trim());
        }

        Map<String, Object> memory = new LinkedHashMap<>();
        if (config.recommendedMemoryMinMb > 0) {
            memory.put("minimum_mb", config.recommendedMemoryMinMb);
        }
        if (config.recommendedMemoryMaxMb > 0) {
            memory.put("maximum_mb", config.recommendedMemoryMaxMb);
        }

        Map<String, Object> modloader = new LinkedHashMap<>();
        modloader.put("type", manifest.modloader.type);
        modloader.put("version", manifest.modloader.version);

        CapabilityEngine.AuthContext accessCtx = authContext(request);
        int effectiveLevel = capabilities.effectiveLevel(accessCtx);

        Map<String, Object> pack = new LinkedHashMap<>();
        pack.put("id", config.serverId + ":pack");
        pack.put("name", manifest.name);
        pack.put("description", config.modpackDescription);
        pack.put("display_version", manifest.version);
        pack.put("current_revision", serverState.getCurrentRevision());
        pack.put("minimum_revision", serverState.getMinimumRevision());
        pack.put("minecraft_version", manifest.minecraftVersion);
        pack.put("modloader", modloader);
        pack.put("file_count", manifest.mods.size() + manifest.overrides.size());
        pack.put("total_size_bytes", totalSize(manifest));
        pack.put("source", manifest.source != null ? manifest.source : config.modpackSource.wireName());
        pack.put("mode", modeWireName(manifest.mode));
        if (manifest.mode == ModpackMode.PLATFORM) {
            Map<String, Object> platform = new LinkedHashMap<>();
            platform.put("source", manifest.source);
            platform.put("project_id", manifest.modpackId);
            platform.put("version_id", manifest.versionId);
            if (manifest.downloadUrl != null && !manifest.downloadUrl.isBlank()) {
                platform.put("download_url", manifest.downloadUrl);
            }
            pack.put("platform", platform);
        } else if (manifest.mode == ModpackMode.ARCHIVE) {
            Map<String, Object> archive = new LinkedHashMap<>();
            archive.put("path", manifest.archivePath);
            archive.put("sha256", manifest.archiveSha256);
            archive.put("size", manifest.archiveSize);
            archive.put("format", manifest.archiveFormat);
            archive.put("download_url", manifest.downloadUrl);
            pack.put("archive", archive);
            pack.put("total_size_bytes", manifest.archiveSize);
        }
        pack.put("access_requires_auth", capabilities.config().accessRequiresAuth);
        pack.put("minimum_access_level", capabilities.minimumLevelFor(AccessCapability.PACK_FETCH));
        if (accessCtx.authenticated || accessCtx.boundUuid != null || accessCtx.passwordAuthenticated) {
            pack.put("effective_level", effectiveLevel);
            pack.put("effective_capabilities", new ArrayList<>(capabilities.capabilities(accessCtx)));
        }
        if (!memory.isEmpty()) {
            pack.put("memory", memory);
        }

        List<String> authMethods = new ArrayList<>();
        if (auth.isOpen()) {
            authMethods.add("open");
        } else {
            authMethods.add("minecraft");
            authMethods.add("link");
            if (auth.hasPassword()) {
                authMethods.add("password");
            }
        }

        Map<String, Object> descriptor = new LinkedHashMap<>();
        descriptor.put("server_id", config.serverId.toString());
        descriptor.put("protocol_version", VestaSync.API_VERSION);
        descriptor.put("capabilities", List.of(
                "server_descriptor",
                "modpack_sync",
                "minecraft_challenge",
                "realtime_ticket"
        ));
        descriptor.put("name", config.serverDisplayName);
        if (config.serverBannerUrl != null && !config.serverBannerUrl.isBlank()) {
            descriptor.put("banner_url", config.serverBannerUrl.trim());
        }
        descriptor.put("minecraft", minecraft);
        descriptor.put("pack", pack);
        descriptor.put("auth_methods", authMethods);
        descriptor.put("updated_at", serverState.getUpdatedAt().toString());
        json(response, callback, descriptor);
    }

    private void handleModpackInfo(Response response, Callback callback) {
        ServerModpackManifest manifest = serverState.getManifest();
        Map<String, Object> info = new LinkedHashMap<>();
        info.put("name", manifest.name);
        info.put("version", manifest.version);
        info.put("current_revision", serverState.getCurrentRevision());
        info.put("minimum_revision", serverState.getMinimumRevision());
        String description = serverState.getModpackDescription();
        if (description != null && !description.trim().isEmpty()) {
            info.put("description", description.trim());
        }
        info.put("mode", modeWireName(manifest.mode));
        info.put("source", manifest.source != null ? manifest.source : config.modpackSource.wireName());
        if (manifest.mode == ModpackMode.PLATFORM) {
            info.put("modpack_id", manifest.modpackId);
            info.put("version_id", manifest.versionId);
            if (manifest.downloadUrl != null && !manifest.downloadUrl.isBlank()) {
                info.put("download_url", manifest.downloadUrl);
            }
        } else if (manifest.mode == ModpackMode.ARCHIVE) {
            info.put("download_url", manifest.downloadUrl);
            info.put("archive_path", manifest.archivePath);
            info.put("archive_sha256", manifest.archiveSha256);
            info.put("archive_size", manifest.archiveSize);
            info.put("archive_format", manifest.archiveFormat);
        }
        info.put("minecraft_version", manifest.minecraftVersion);
        Map<String, String> ml = new LinkedHashMap<>();
        ml.put("type", manifest.modloader.type);
        ml.put("version", manifest.modloader.version);
        info.put("modloader", ml);
        info.put("file_count", manifest.mods.size() + manifest.overrides.size());
        info.put(
                "total_size_bytes",
                manifest.mode == ModpackMode.ARCHIVE ? manifest.archiveSize : totalSize(manifest)
        );
        info.put("updated_at", serverState.getUpdatedAt().toString());
        info.put("security_mode", auth.mode().name().toLowerCase());
        if (config.recommendedMemoryMinMb > 0 || config.recommendedMemoryMaxMb > 0) {
            Map<String, Integer> memory = new LinkedHashMap<>();
            if (config.recommendedMemoryMinMb > 0) {
                memory.put("minimum_mb", config.recommendedMemoryMinMb);
            }
            if (config.recommendedMemoryMaxMb > 0) {
                memory.put("maximum_mb", config.recommendedMemoryMaxMb);
            }
            info.put("memory", memory);
        }
        json(response, callback, info);
    }

    private void handleModpackManifest(Response response, Callback callback) {
        json(response, callback, serverState.getManifest());
    }

    private void handleModpackArchive(Response response, Callback callback) throws Exception {
        Optional<LocalArchivePack> archive = serverState.localArchive();
        if (archive.isEmpty()) {
            response.setStatus(404);
            json(response, callback, Map.of("error", "no local archive pack configured"));
            return;
        }
        LocalArchivePack pack = archive.get();
        response.getHeaders().put(HttpHeader.CONTENT_TYPE, "application/octet-stream");
        response.getHeaders().put(HttpHeader.CONTENT_LENGTH, String.valueOf(pack.size));
        response.getHeaders().put(HttpHeader.ETAG, "\"" + pack.sha256 + "\"");
        response.getHeaders().put("X-Vesta-Archive-Sha256", pack.sha256);
        response.getHeaders().put("X-Vesta-Archive-Format", pack.format);
        response.setStatus(200);
        response.write(true, ByteBuffer.wrap(Files.readAllBytes(pack.resolvedPath)), callback);
    }

    private static String modeWireName(ModpackMode mode) {
        if (mode == null) {
            return "custom";
        }
        switch (mode) {
            case PLATFORM:
                return "platform";
            case ARCHIVE:
                return "archive";
            case CUSTOM:
            default:
                return "custom";
        }
    }

    private void handleModpackVersions(Response response, Callback callback) {
        json(response, callback, Map.of("versions", serverState.listVersions()));
    }

    private void handleVersionManifest(Request request, Response response, Callback callback) {
        String path = request.getHttpURI().getPath();
        String prefix = VestaSync.API_MODPACK_VERSIONS + "/";
        String suffix = "/manifest";
        String version = path.substring(prefix.length(), path.length() - suffix.length());
        if (!version.equals(serverState.getCurrentVersion())) {
            response.setStatus(404);
            json(response, callback, Map.of("error", "version not found"));
            return;
        }
        response.setStatus(200);
        json(response, callback, serverState.getManifest());
    }

    private void handleSyncCheck(Request request, Response response, Callback callback) throws Exception {
        if (!HttpMethod.POST.is(request.getMethod())) {
            response.setStatus(405);
            json(response, callback, Map.of("error", "method not allowed"));
            return;
        }

        String body = readBody(request);
        JsonObject req = GSON.fromJson(body, JsonObject.class);
        String clientVersion = req.has("current_version") ? req.get("current_version").getAsString() : "";

        Map<String, String> installedHashes = new LinkedHashMap<>();
        if (req.has("installed_hashes") && req.get("installed_hashes").isJsonObject()) {
            JsonObject hashes = req.getAsJsonObject("installed_hashes");
            for (String key : hashes.keySet()) {
                installedHashes.put(key, hashes.get(key).getAsString());
            }
        }

        SyncCheckResult result = serverState.syncCheck(clientVersion, installedHashes);

        Map<String, Object> responseBody = new LinkedHashMap<>();
        responseBody.put("needs_update", result.needsUpdate);
        if (result.needsUpdate) {
            responseBody.put("new_version", result.newVersion);
            responseBody.put("actions", result.actions);
            if (result.manifest != null) {
                responseBody.put("manifest", result.manifest);
            }
        }
        response.setStatus(200);
        json(response, callback, responseBody);
    }

    private void handleFileDownload(Request request, Response response, Callback callback) throws Exception {
        String sha256 = request.getHttpURI().getPath().substring(VestaSync.API_FILES.length());
        if (sha256.isEmpty() || sha256.contains("/")) {
            response.setStatus(400);
            json(response, callback, Map.of("error", "invalid hash"));
            return;
        }

        ServerModpackManifest manifest = serverState.getManifest();
        List<ManifestFile> all = new ArrayList<>(manifest.mods);
        all.addAll(manifest.overrides);
        Optional<Path> live = serverState.installer().resolveByHash(sha256, all);
        if (live.isEmpty()) {
            response.setStatus(404);
            json(response, callback, Map.of("error", "file not found"));
            return;
        }
        serveFile(response, callback, live.get());
    }

    private void serveFile(Response response, Callback callback, Path file) throws Exception {
        response.getHeaders().put(HttpHeader.CONTENT_TYPE, "application/octet-stream");
        response.getHeaders().put(HttpHeader.CONTENT_LENGTH, String.valueOf(Files.size(file)));
        response.setStatus(200);
        response.write(true, ByteBuffer.wrap(Files.readAllBytes(file)), callback);
    }

    private static void configureRealtime(ServerWebSocketContainer container) {
        container.setIdleTimeout(Duration.ofSeconds(45));
        container.setMaxTextMessageSize(64 * 1024);
        container.setMaxBinaryMessageSize(64 * 1024);
        container.setMaxFrameSize(64 * 1024);
    }

    private static boolean isAllowedRealtimeOrigin(String origin) {
        if (origin == null || origin.trim().isEmpty()) {
            return true;
        }
        String normalized = origin.trim().toLowerCase(Locale.ROOT);
        if (normalized.equals("tauri://localhost")
                || normalized.equals("http://tauri.localhost")
                || normalized.equals("https://tauri.localhost")) {
            return true;
        }
        return normalized.matches("https?://(localhost|127\\.0\\.0\\.1|\\[::1\\])(?::[0-9]+)?");
    }

    private void json(Response response, Callback callback, Object obj) {
        byte[] bytes = GSON.toJson(obj).getBytes(StandardCharsets.UTF_8);
        response.getHeaders().put(HttpHeader.CONTENT_TYPE, "application/json");
        response.write(true, ByteBuffer.wrap(bytes), callback);
    }

    private void applyCors(Request request, Response response) {
        String origin = request.getHeaders().get(HttpHeader.ORIGIN);
        response.getHeaders().put(HttpHeader.ACCESS_CONTROL_ALLOW_ORIGIN, origin != null ? origin : "*");
        response.getHeaders().put(HttpHeader.ACCESS_CONTROL_ALLOW_METHODS, "GET, POST, PUT, DELETE, OPTIONS");
        response.getHeaders().put(HttpHeader.ACCESS_CONTROL_ALLOW_HEADERS, "Authorization, Content-Type, " + VestaSync.SESSION_HEADER);
        response.getHeaders().put(HttpHeader.ACCESS_CONTROL_MAX_AGE, "86400");
        if (origin != null) {
            response.getHeaders().put(HttpHeader.VARY, "Origin");
        }
    }

    private void serveDevtools(Response response, Callback callback) throws Exception {
        try (InputStream in = VestaHttpServer.class.getResourceAsStream("/devtools/index.html")) {
            if (in == null) {
                response.setStatus(404);
                json(response, callback, Map.of("error", "devtools page missing"));
                return;
            }
            byte[] bytes = in.readAllBytes();
            response.setStatus(200);
            response.getHeaders().put(HttpHeader.CONTENT_TYPE, "text/html; charset=utf-8");
            response.getHeaders().put(HttpHeader.CACHE_CONTROL, "no-store");
            response.write(true, ByteBuffer.wrap(bytes), callback);
        }
    }

    private String readBody(Request request) throws Exception {
        return Content.Source.asString(request);
    }

    private static UUID parseUuid(String raw) {
        String compact = raw == null ? "" : raw.trim().replace("-", "");
        if (compact.length() != 32) {
            throw new IllegalArgumentException("invalid minecraft profile UUID");
        }
        return UUID.fromString(
                compact.substring(0, 8) + "-"
                        + compact.substring(8, 12) + "-"
                        + compact.substring(12, 16) + "-"
                        + compact.substring(16, 20) + "-"
                        + compact.substring(20)
        );
    }

    private long totalSize(ServerModpackManifest manifest) {
        long total = 0;
        for (ManifestFile m : manifest.mods) total += m.size;
        for (ManifestFile o : manifest.overrides) total += o.size;
        return total;
    }
}
