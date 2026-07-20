package com.vesta.loader.server;

import com.google.gson.Gson;
import com.google.gson.GsonBuilder;
import com.google.gson.FieldNamingPolicy;
import com.google.gson.JsonObject;
import com.sun.net.httpserver.HttpExchange;
import com.sun.net.httpserver.HttpServer;
import com.vesta.loader.ModLog;
import com.vesta.sync.FileStore;
import com.vesta.sync.VestaSync;
import com.vesta.sync.manifest.KnownSourceRegistry;
import com.vesta.sync.manifest.ModloaderInfo;
import com.vesta.sync.manifest.ManifestBuilder;
import com.vesta.sync.manifest.ModpackState;
import com.vesta.sync.manifest.ServerInfo;
import com.vesta.sync.manifest.UnresolvedPolicy;
import com.vesta.sync.manifest.ServerModpackManifest;
import com.vesta.sync.sync.SyncCheckResult;

import java.io.ByteArrayOutputStream;
import java.io.IOException;
import java.io.InputStream;
import java.io.OutputStream;
import java.net.InetSocketAddress;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import java.util.concurrent.Executors;

public final class ModHttpServer {

    private static final Gson GSON = new GsonBuilder()
            .setFieldNamingPolicy(FieldNamingPolicy.LOWER_CASE_WITH_UNDERSCORES)
            .create();

    private final HttpServer server;
    private final FileStore fileStore;
    private final ModpackState modpackState;
    private final String host;
    private final int mcPort;

    public ModHttpServer(
            int port,
            FileStore fileStore,
            Path serverRoot,
            String modpackName,
            String modpackVersion,
            String mcVersion,
            String loaderType,
            String loaderVersion,
            String host,
            int mcPort
    ) throws IOException {
        this.fileStore = fileStore;
        this.host = host;
        this.mcPort = mcPort;

        ModloaderInfo modloader = new ModloaderInfo();
        modloader.type = loaderType;
        modloader.version = loaderVersion;

        ServerInfo serverInfo = null;
        if (host != null && !host.isBlank()) {
            serverInfo = new ServerInfo();
            serverInfo.host = host;
            serverInfo.port = mcPort;
            serverInfo.description = "Join this server";
            serverInfo.isRequired = true;
        }

        try {
            this.modpackState = ModpackState.fromScan(
                    serverRoot,
                    ManifestBuilder.DEFAULT_SCAN_DIRS,
                    KnownSourceRegistry.load(serverRoot),
                    UnresolvedPolicy.SERVER,
                    List.of(),
                    modpackName,
                    modpackVersion,
                    mcVersion,
                    modloader,
                    serverInfo
            );
        } catch (IOException e) {
            ModLog.error("Failed to scan server files for manifest", e);
            throw e;
        }

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
        ServerModpackManifest manifest = modpackState.getManifest();
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
        sendJson(ex, 200, info);
    }

    private void handleManifest(HttpExchange ex) throws IOException {
        sendJson(ex, 200, modpackState.getManifest());
    }

    private void handleSyncCheck(HttpExchange ex) throws IOException {
        if (!"POST".equals(ex.getRequestMethod())) {
            sendJson(ex, 405, Map.of("error", "method not allowed"));
            return;
        }

        String body = readBody(ex);
        JsonObject req = GSON.fromJson(body, JsonObject.class);
        String clientVersion = req.has("current_version") ? req.get("current_version").getAsString() : "";

        Map<String, String> installedHashes = new LinkedHashMap<>();
        if (req.has("installed_hashes") && req.get("installed_hashes").isJsonObject()) {
            JsonObject hashes = req.getAsJsonObject("installed_hashes");
            for (String key : hashes.keySet()) {
                installedHashes.put(key, hashes.get(key).getAsString());
            }
        }

        SyncCheckResult result = modpackState.syncCheck(clientVersion, installedHashes);

        Map<String, Object> response = new LinkedHashMap<>();
        response.put("needs_update", result.needsUpdate);
        if (result.needsUpdate) {
            response.put("new_version", result.newVersion);
            response.put("actions", result.actions);
            if (result.manifest != null) {
                response.put("manifest", result.manifest);
            }
        }
        sendJson(ex, 200, response);
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

    private String readBody(HttpExchange ex) throws IOException {
        InputStream in = ex.getRequestBody();
        ByteArrayOutputStream bos = new ByteArrayOutputStream();
        byte[] buf = new byte[4096];
        int read;
        while ((read = in.read(buf)) != -1) {
            bos.write(buf, 0, read);
        }
        return bos.toString(StandardCharsets.UTF_8);
    }

    private long totalSize(ServerModpackManifest manifest) {
        long total = 0;
        for (var m : manifest.mods) total += m.size;
        for (var o : manifest.overrides) total += o.size;
        return total;
    }
}
