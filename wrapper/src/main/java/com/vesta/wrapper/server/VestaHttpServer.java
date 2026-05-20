package com.vesta.wrapper.server;

import com.google.gson.Gson;
import com.google.gson.GsonBuilder;
import com.google.gson.JsonObject;
import com.google.gson.FieldNamingPolicy;
import com.vesta.sync.VestaSync;
import com.vesta.sync.manifest.ServerModpackManifest;
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

import java.nio.ByteBuffer;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.ArrayList;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;

public final class VestaHttpServer {

    private static final Logger log = LoggerFactory.getLogger(VestaHttpServer.class);
    private static final Gson GSON = new GsonBuilder()
            .setFieldNamingPolicy(FieldNamingPolicy.LOWER_CASE_WITH_UNDERSCORES)
            .setPrettyPrinting()
            .create();

    private final Server jetty;
    private final ServerState serverState;
    private final FileStore fileStore;

    public VestaHttpServer(int port, ServerState serverState, FileStore fileStore) {
        this.serverState = serverState;
        this.fileStore = fileStore;
        this.jetty = new Server();
        ServerConnector connector = new ServerConnector(jetty);
        connector.setPort(port);
        jetty.addConnector(connector);
        jetty.setHandler(new Handler.Abstract() {
            @Override
            public boolean handle(Request request, Response response, Callback callback) throws Exception {
                handleRequest(request, response, callback);
                return true;
            }
        });
    }

    public VestaHttpServer(ServerState serverState, FileStore fileStore) {
        this(VestaSync.DEFAULT_HTTP_PORT, serverState, fileStore);
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
        String method = request.getMethod();

        try {
            switch (path) {
                case VestaSync.API_MODPACK_INFO:
                    handleModpackInfo(request, response, callback);
                    return;
                case VestaSync.API_MANIFEST:
                    handleModpackManifest(request, response, callback);
                    return;
                case VestaSync.API_MODPACK_VERSIONS:
                    handleModpackVersions(request, response, callback);
                    return;
                case VestaSync.API_SYNC_CHECK:
                    handleSyncCheck(request, response, callback);
                    return;
                default:
                    if (path.startsWith(VestaSync.API_FILES)) {
                        handleFileDownload(request, response, callback);
                        return;
                    }
                    if (path.startsWith(VestaSync.API_MODPACK_VERSIONS + "/")
                            && path.endsWith("/manifest")) {
                        handleVersionManifest(request, response, callback);
                        return;
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

    private void handleModpackInfo(Request request, Response response, Callback callback) {
        ServerModpackManifest manifest = serverState.getManifest();
        Map<String, Object> info = new LinkedHashMap<>();
        info.put("name", manifest.name);
        info.put("version", manifest.version);
        info.put("mode", manifest.mode);
        info.put("source", "server");
        info.put("minecraft_version", manifest.minecraftVersion);
        Map<String, String> ml = new LinkedHashMap<>();
        ml.put("type", manifest.modloader.type);
        ml.put("version", manifest.modloader.version);
        info.put("modloader", ml);
        info.put("file_count", manifest.mods.size() + manifest.overrides.size());
        info.put("total_size_bytes", totalSize(manifest));
        info.put("updated_at", "2026-05-20T00:00:00Z");
        json(response, callback, info);
    }

    private void handleModpackManifest(Request request, Response response, Callback callback) {
        json(response, callback, serverState.getManifest());
    }

    private void handleModpackVersions(Request request, Response response, Callback callback) {
        List<Map<String, String>> versions = new ArrayList<>();
        Map<String, String> v = new LinkedHashMap<>();
        v.put("version", serverState.getCurrentVersion());
        v.put("published_at", "2026-05-20T00:00:00Z");
        versions.add(v);
        json(response, callback, Map.of("versions", versions));
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
        String latestVersion = serverState.getCurrentVersion();
        boolean needsUpdate = !latestVersion.equals(clientVersion);

        Map<String, Object> result = new LinkedHashMap<>();
        result.put("needs_update", needsUpdate);
        if (needsUpdate) {
            result.put("new_version", latestVersion);
        }
        json(response, callback, result);
    }

    private void handleFileDownload(Request request, Response response, Callback callback) throws Exception {
        String sha256 = request.getHttpURI().getPath().substring(VestaSync.API_FILES.length());
        if (sha256.isEmpty() || sha256.contains("/")) {
            response.setStatus(400);
            json(response, callback, Map.of("error", "invalid hash"));
            return;
        }

        if (!fileStore.exists(sha256)) {
            response.setStatus(404);
            json(response, callback, Map.of("error", "file not found"));
            return;
        }

        Path file = fileStore.get(sha256);
        response.getHeaders().put(HttpHeader.CONTENT_TYPE, "application/octet-stream");
        response.getHeaders().put(HttpHeader.CONTENT_LENGTH, String.valueOf(Files.size(file)));
        response.setStatus(200);
        response.write(true, ByteBuffer.wrap(Files.readAllBytes(file)), callback);
    }

    private void json(Response response, Callback callback, Object obj) {
        byte[] bytes = GSON.toJson(obj).getBytes(StandardCharsets.UTF_8);
        response.getHeaders().put(HttpHeader.CONTENT_TYPE, "application/json");
        response.setStatus(200);
        response.write(true, ByteBuffer.wrap(bytes), callback);
    }

    private String readBody(Request request) throws Exception {
        StringBuilder sb = new StringBuilder();
        while (true) {
            Content.Chunk chunk = request.read();
            if (chunk == null) break;
            if (Content.Chunk.isFailure(chunk)) {
                chunk.release();
                throw new java.io.IOException("Failed to read request body");
            }
            java.nio.ByteBuffer buf = chunk.getByteBuffer();
            if (buf != null && buf.hasRemaining()) {
                byte[] bytes = new byte[buf.remaining()];
                buf.get(bytes);
                sb.append(new String(bytes, StandardCharsets.UTF_8));
            }
            chunk.release();
        }
        return sb.toString();
    }

    private long totalSize(ServerModpackManifest manifest) {
        long total = 0;
        for (var m : manifest.mods) total += m.size;
        for (var o : manifest.overrides) total += o.size;
        return total;
    }
}
