package com.vesta.sync.auth;

import com.google.gson.JsonArray;
import com.google.gson.JsonElement;
import com.google.gson.JsonObject;
import com.google.gson.JsonParser;
import org.slf4j.Logger;
import org.slf4j.LoggerFactory;

import java.io.IOException;
import java.io.Reader;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.Collections;
import java.util.HashMap;
import java.util.Locale;
import java.util.Map;
import java.util.UUID;

/**
 * Reads Minecraft {@code ops.json} and exposes numeric OP levels (0–4) directly.
 */
public final class OpsJsonReader implements OpsLevelProvider {

    private static final Logger log = LoggerFactory.getLogger(OpsJsonReader.class);

    private final Path opsFile;
    private volatile Map<UUID, Integer> levels = Collections.emptyMap();
    private volatile long lastModified = Long.MIN_VALUE;

    public OpsJsonReader(Path serverRoot) {
        this.opsFile = serverRoot.toAbsolutePath().normalize().resolve("ops.json");
        reloadIfNeeded();
    }

    @Override
    public int level(UUID uuid) {
        if (uuid == null) {
            return 0;
        }
        reloadIfNeeded();
        Integer level = levels.get(uuid);
        return level == null ? 0 : level.intValue();
    }

    private void reloadIfNeeded() {
        try {
            if (!Files.isRegularFile(opsFile)) {
                levels = Collections.emptyMap();
                lastModified = Long.MIN_VALUE;
                return;
            }
            long modified = Files.getLastModifiedTime(opsFile).toMillis();
            if (modified == lastModified) {
                return;
            }
            Map<UUID, Integer> next = new HashMap<UUID, Integer>();
            try (Reader reader = Files.newBufferedReader(opsFile, StandardCharsets.UTF_8)) {
                JsonElement root = new JsonParser().parse(reader);
                if (root != null && root.isJsonArray()) {
                    JsonArray array = root.getAsJsonArray();
                    for (JsonElement element : array) {
                        if (!element.isJsonObject()) {
                            continue;
                        }
                        JsonObject object = element.getAsJsonObject();
                        if (!object.has("uuid")) {
                            continue;
                        }
                        UUID uuid = parseUuid(object.get("uuid").getAsString());
                        if (uuid == null) {
                            continue;
                        }
                        int level = object.has("level") ? object.get("level").getAsInt() : 4;
                        if (level < 0) {
                            level = 0;
                        }
                        if (level > 4) {
                            level = 4;
                        }
                        next.put(uuid, Integer.valueOf(level));
                    }
                }
            }
            levels = Collections.unmodifiableMap(next);
            lastModified = modified;
        } catch (IOException | RuntimeException e) {
            log.warn("Failed to read {}: {}", opsFile, e.getMessage());
        }
    }

    private static UUID parseUuid(String raw) {
        if (raw == null || raw.trim().isEmpty()) {
            return null;
        }
        String value = raw.trim().toLowerCase(Locale.ROOT);
        try {
            return UUID.fromString(value);
        } catch (IllegalArgumentException ignored) {
            String compact = value.replace("-", "");
            if (compact.length() != 32) {
                return null;
            }
            String dashed = compact.substring(0, 8) + "-"
                    + compact.substring(8, 12) + "-"
                    + compact.substring(12, 16) + "-"
                    + compact.substring(16, 20) + "-"
                    + compact.substring(20);
            try {
                return UUID.fromString(dashed);
            } catch (IllegalArgumentException e) {
                return null;
            }
        }
    }
}
