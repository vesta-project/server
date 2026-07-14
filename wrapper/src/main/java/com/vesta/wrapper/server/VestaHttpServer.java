package com.vesta.wrapper.server;

import com.google.gson.FieldNamingPolicy;
import com.google.gson.Gson;
import com.google.gson.GsonBuilder;
import com.google.gson.JsonObject;
import com.vesta.sync.VestaSync;
import com.vesta.sync.auth.AuthGate;
import com.vesta.sync.auth.BearerTokenStore;
import com.vesta.sync.auth.CapabilityEngine;
import com.vesta.sync.auth.LinkCodeService;
import com.vesta.sync.auth.MsaProfileVerifier;
import com.vesta.sync.manifest.ManifestFile;
import com.vesta.sync.manifest.PrivateFileEntry;
import com.vesta.sync.manifest.ServerModpackManifest;
import com.vesta.sync.sync.SyncCheckResult;
import com.vesta.wrapper.auth.AuthService;
import com.vesta.wrapper.content.ContentInstaller;
import com.vesta.wrapper.security.WhitelistService;
import org.eclipse.jetty.http.HttpHeader;
import org.eclipse.jetty.http.HttpMethod;
import org.eclipse.jetty.io.Content;
import org.eclipse.jetty.server.Handler;
import org.eclipse.jetty.server.Request;
import org.eclipse.jetty.server.Response;
import org.eclipse.jetty.server.Server;
import org.eclipse.jetty.server.ServerConnector;
import org.eclipse.jetty.util.Callback;
import org.slf4j.Logger;
import org.slf4j.LoggerFactory;

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
import java.util.Map;
import java.util.Optional;
import java.util.UUID;

public final class VestaHttpServer {

    private static final Logger log = LoggerFactory.getLogger(VestaHttpServer.class);
    private static final Gson GSON = new GsonBuilder()
            .setFieldNamingPolicy(FieldNamingPolicy.LOWER_CASE_WITH_UNDERSCORES)
            .setPrettyPrinting()
            .create();
    private static final MsaProfileVerifier MSA = new MsaProfileVerifier();

    private final Server jetty;
    private final ServerState serverState;
    private final AuthService auth;
    private final LinkCodeService linkCodes;
    private final CapabilityEngine capabilities;
    private final WhitelistService whitelist;

    public VestaHttpServer(
            int port,
            ServerState serverState,
            AuthService auth,
            LinkCodeService linkCodes,
            CapabilityEngine capabilities,
            WhitelistService whitelist,
            boolean bindLocalOnly
    ) {
        this.serverState = serverState;
        this.auth = auth;
        this.linkCodes = linkCodes;
        this.capabilities = capabilities;
        this.whitelist = whitelist;
        this.jetty = new Server();
        ServerConnector connector = new ServerConnector(jetty);
        connector.setPort(port);
        if (bindLocalOnly) {
            connector.setHost("127.0.0.1");
        }
        jetty.addConnector(connector);
        jetty.setHandler(new Handler.Abstract() {
            @Override
            public boolean handle(Request request, Response response, Callback callback) throws Exception {
                handleRequest(request, response, callback);
                return true;
            }
        });
    }

    public void start() throws Exception {
        jetty.start();
        log.info("HTTP REST API listening on port {}", ((ServerConnector) jetty.getConnectors()[0]).getPort());
    }

    public void stop() throws Exception {
        jetty.stop();
        log.info("HTTP REST API stopped");
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
                case VestaSync.API_AUTH_MSA -> {
                    handleAuthMsa(request, response, callback);
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
                case VestaSync.API_PRIVATE_FILES -> {
                    if (!requireCapability(request, response, callback, CapabilityEngine.Action.PRIVATE_RESOURCES)) return;
                    handlePrivateList(response, callback);
                    return;
                }
                default -> {
                    if (path.startsWith(VestaSync.API_FILES)) {
                        if (!requireCapability(request, response, callback, CapabilityEngine.Action.PUBLIC_MODPACK)) return;
                        handleFileDownload(request, response, callback);
                        return;
                    }
                    if (path.startsWith(VestaSync.API_PRIVATE_FILES + "/")) {
                        if (!requireCapability(request, response, callback, CapabilityEngine.Action.PRIVATE_RESOURCES)) return;
                        handlePrivateDownload(request, response, callback);
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
        CapabilityEngine.Decision d = capabilities.check(action, authContext(request));
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

    private void handleAuthMsa(Request request, Response response, Callback callback) throws Exception {
        if (!HttpMethod.POST.is(request.getMethod())) {
            response.setStatus(405);
            json(response, callback, Map.of("error", "method not allowed"));
            return;
        }
        if (auth.isOpen()) {
            response.setStatus(400);
            json(response, callback, Map.of("error", "MSA auth not used in open mode"));
            return;
        }
        JsonObject body = GSON.fromJson(readBody(request), JsonObject.class);
        String accessToken = body != null && body.has("access_token") ? body.get("access_token").getAsString() : "";
        Optional<MsaProfileVerifier.Profile> profile = MSA.verify(accessToken);
        if (profile.isEmpty()) {
            response.setStatus(401);
            json(response, callback, Map.of("error", "invalid minecraft access token"));
            return;
        }
        String token = auth.issueMsaToken(profile.get().uuid, profile.get().name);
        Map<String, Object> out = new LinkedHashMap<>();
        out.put("token", token);
        out.put("token_type", "Bearer");
        out.put("uuid", profile.get().uuid.toString());
        out.put("name", profile.get().name);
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

    private void handlePrivateList(Response response, Callback callback) {
        List<Map<String, Object>> files = new ArrayList<>();
        for (PrivateFileEntry e : serverState.getPrivateFiles()) {
            Map<String, Object> row = new LinkedHashMap<>();
            row.put("path", e.path);
            row.put("sha256", e.sha256);
            row.put("size", e.size);
            files.add(row);
        }
        json(response, callback, Map.of("files", files));
    }

    private void handlePrivateDownload(Request request, Response response, Callback callback) throws Exception {
        String rel = request.getHttpURI().getPath().substring(VestaSync.API_PRIVATE_FILES.length());
        if (rel.startsWith("/")) {
            rel = rel.substring(1);
        }
        rel = URLDecoder.decode(rel, StandardCharsets.UTF_8);
        if (rel.isEmpty()) {
            response.setStatus(400);
            json(response, callback, Map.of("error", "path required"));
            return;
        }
        boolean allowed = false;
        for (PrivateFileEntry e : serverState.getPrivateFiles()) {
            if (e.path.equals(rel)) {
                allowed = true;
                break;
            }
        }
        if (!allowed) {
            response.setStatus(404);
            json(response, callback, Map.of("error", "private file not found"));
            return;
        }
        Optional<Path> live = serverState.installer().resolveLivePath(rel);
        if (live.isEmpty()) {
            response.setStatus(404);
            json(response, callback, Map.of("error", "file missing on disk"));
            return;
        }
        serveFile(response, callback, live.get());
    }

    private void handleStatus(Response response, Callback callback) {
        Map<String, Object> status = new LinkedHashMap<>();
        status.put("security_mode", auth.mode().name().toLowerCase());
        status.put("modpack_version", serverState.getCurrentVersion());
        status.put("updated_at", serverState.getUpdatedAt().toString());
        status.put("private_file_count", serverState.getPrivateFiles().size());
        status.put("whitelist_count", whitelist.all().size());
        json(response, callback, status);
    }

    private void handleModpackInfo(Response response, Callback callback) {
        ServerModpackManifest manifest = serverState.getManifest();
        Map<String, Object> info = new LinkedHashMap<>();
        info.put("name", manifest.name);
        info.put("version", manifest.version);
        String description = serverState.getModpackDescription();
        if (description != null && !description.trim().isEmpty()) {
            info.put("description", description.trim());
        }
        info.put("mode", manifest.mode);
        info.put("source", "server");
        info.put("minecraft_version", manifest.minecraftVersion);
        Map<String, String> ml = new LinkedHashMap<>();
        ml.put("type", manifest.modloader.type);
        ml.put("version", manifest.modloader.version);
        info.put("modloader", ml);
        info.put("file_count", manifest.mods.size() + manifest.overrides.size());
        info.put("total_size_bytes", totalSize(manifest));
        info.put("updated_at", serverState.getUpdatedAt().toString());
        info.put("security_mode", auth.mode().name().toLowerCase());
        json(response, callback, info);
    }

    private void handleModpackManifest(Response response, Callback callback) {
        json(response, callback, serverState.getManifest());
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
        Optional<Path> live = serverState.installer().resolveByHash(
                sha256, serverState.getPrivateFiles(), all);
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

    private void json(Response response, Callback callback, Object obj) {
        byte[] bytes = GSON.toJson(obj).getBytes(StandardCharsets.UTF_8);
        response.getHeaders().put(HttpHeader.CONTENT_TYPE, "application/json");
        response.write(true, ByteBuffer.wrap(bytes), callback);
    }

    private void applyCors(Request request, Response response) {
        String origin = request.getHeaders().get(HttpHeader.ORIGIN);
        response.getHeaders().put(HttpHeader.ACCESS_CONTROL_ALLOW_ORIGIN, origin != null ? origin : "*");
        response.getHeaders().put(HttpHeader.ACCESS_CONTROL_ALLOW_METHODS, "GET, POST, DELETE, OPTIONS");
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

    private long totalSize(ServerModpackManifest manifest) {
        long total = 0;
        for (ManifestFile m : manifest.mods) total += m.size;
        for (ManifestFile o : manifest.overrides) total += o.size;
        return total;
    }
}
