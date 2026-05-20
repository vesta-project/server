package com.vesta.loader.server;

import com.google.gson.Gson;
import com.google.gson.GsonBuilder;
import com.google.gson.FieldNamingPolicy;
import com.sun.net.httpserver.HttpExchange;
import com.sun.net.httpserver.HttpServer;
import com.vesta.loader.ModLog;
import com.vesta.sync.FileStore;
import com.vesta.sync.VestaSync;

import java.io.ByteArrayOutputStream;
import java.io.IOException;
import java.io.InputStream;
import java.io.OutputStream;
import java.net.InetSocketAddress;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.LinkedHashMap;
import java.util.Map;
import java.util.concurrent.Executors;

public final class ModHttpServer {

    private static final Gson GSON = new GsonBuilder()
            .setFieldNamingPolicy(FieldNamingPolicy.LOWER_CASE_WITH_UNDERSCORES)
            .create();

    private final HttpServer server;
    private final FileStore fileStore;
    private final String mcVersion;
    private final String loaderType;
    private final String host;
    private final int mcPort;

    public ModHttpServer(int port, FileStore fileStore, String mcVersion,
                         String loaderType, String host, int mcPort) throws IOException {
        this.fileStore = fileStore;
        this.mcVersion = mcVersion;
        this.loaderType = loaderType;
        this.host = host;
        this.mcPort = mcPort;
        this.server = HttpServer.create(new InetSocketAddress(port), 0);
        this.server.setExecutor(Executors.newFixedThreadPool(4, r -> {
            Thread t = new Thread(r, "vesta-mod-http");
            t.setDaemon(true);
            return t;
        }));
        registerHandlers();
    }

    private void registerHandlers() {
        server.createContext(VestaSync.API_MODPACK_INFO, this::handleInfo);
        server.createContext(VestaSync.API_MANIFEST, this::handleManifest);
        server.createContext(VestaSync.API_SYNC_CHECK, this::handleSyncCheck);
        server.createContext(VestaSync.API_FILES, this::handleFile);
    }

    public void start() {
        server.start();
    }

    public void stop() {
        server.stop(0);
    }

    private void handleInfo(HttpExchange ex) throws IOException {
        Map<String, Object> info = new LinkedHashMap<>();
        info.put("name", "Server Modpack");
        info.put("version", "1.0.0");
        info.put("mode", "custom");
        info.put("source", "server");
        info.put("minecraft_version", mcVersion);
        Map<String, String> ml = new LinkedHashMap<>();
        ml.put("type", loaderType);
        ml.put("version", "latest");
        info.put("modloader", ml);
        info.put("file_count", countFiles());
        info.put("total_size_bytes", 0);
        info.put("updated_at", "2026-05-20T00:00:00Z");
        sendJson(ex, 200, info);
    }

    private void handleManifest(HttpExchange ex) throws IOException {
        Map<String, Object> manifest = new LinkedHashMap<>();
        manifest.put("manifest_version", 1);
        manifest.put("mode", "custom");
        manifest.put("name", "Server Modpack");
        manifest.put("version", "1.0.0");
        manifest.put("minecraft_version", mcVersion);
        Map<String, String> ml = new LinkedHashMap<>();
        ml.put("type", loaderType);
        ml.put("version", "latest");
        manifest.put("modloader", ml);
        manifest.put("mods", java.util.Collections.emptyList());
        manifest.put("overrides", java.util.Collections.emptyList());
        if (host != null && !host.isEmpty()) {
            Map<String, Object> si = new LinkedHashMap<>();
            si.put("host", host);
            si.put("port", mcPort);
            si.put("description", "Join this server");
            si.put("is_required", true);
            manifest.put("server_info", si);
        }
        sendJson(ex, 200, manifest);
    }

    private void handleSyncCheck(HttpExchange ex) throws IOException {
        if (!"POST".equals(ex.getRequestMethod())) {
            sendJson(ex, 405, Map.of("error", "method not allowed"));
            return;
        }
        InputStream in = ex.getRequestBody();
        ByteArrayOutputStream bos = new ByteArrayOutputStream();
        byte[] buf = new byte[4096];
        int read;
        while ((read = in.read(buf)) != -1) {
            bos.write(buf, 0, read);
        }
        String body = bos.toString(StandardCharsets.UTF_8);
        com.google.gson.JsonObject req = GSON.fromJson(body, com.google.gson.JsonObject.class);
        String clientVersion = req.has("current_version") ? req.get("current_version").getAsString() : "";
        boolean needsUpdate = !"1.0.0".equals(clientVersion);

        Map<String, Object> result = new LinkedHashMap<>();
        result.put("needs_update", needsUpdate);
        if (needsUpdate) {
            result.put("new_version", "1.0.0");
        }
        sendJson(ex, 200, result);
    }

    private void handleFile(HttpExchange ex) throws IOException {
        String sha256 = ex.getRequestURI().getPath().substring(VestaSync.API_FILES.length());
        if (sha256.isEmpty() || sha256.contains("/")) {
            sendJson(ex, 400, Map.of("error", "invalid hash"));
            return;
        }

        Path file = fileStore.get(sha256);
        if (file == null) {
            sendJson(ex, 404, Map.of("error", "file not found"));
            return;
        }

        byte[] data = Files.readAllBytes(file);
        ex.getResponseHeaders().set("Content-Type", "application/octet-stream");
        ex.sendResponseHeaders(200, data.length);
        OutputStream os = ex.getResponseBody();
        os.write(data);
        os.close();
    }

    private void sendJson(HttpExchange ex, int status, Object data) throws IOException {
        byte[] bytes = GSON.toJson(data).getBytes(StandardCharsets.UTF_8);
        ex.getResponseHeaders().set("Content-Type", "application/json; charset=utf-8");
        ex.sendResponseHeaders(status, bytes.length);
        OutputStream os = ex.getResponseBody();
        os.write(bytes);
        os.close();
    }

    private int countFiles() {
        try {
            return fileStore.count();
        } catch (Exception e) {
            return 0;
        }
    }
}
