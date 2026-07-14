package com.vesta.wrapper.security;

import com.google.gson.Gson;
import com.google.gson.JsonArray;
import com.google.gson.JsonElement;
import com.google.gson.JsonObject;
import org.slf4j.Logger;
import org.slf4j.LoggerFactory;

import java.io.IOException;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.Collections;
import java.util.HashSet;
import java.util.Set;
import java.util.UUID;

/**
 * Reads Minecraft {@code whitelist.json} from the scan root when present.
 */
public final class WhitelistService {

    private static final Logger log = LoggerFactory.getLogger(WhitelistService.class);
    private static final Gson GSON = new Gson();

    private final Path whitelistFile;
    private volatile Set<UUID> cached = Collections.emptySet();

    public WhitelistService(Path scanRoot) {
        this.whitelistFile = scanRoot.resolve("whitelist.json");
        reload();
    }

    public void reload() {
        if (!Files.isRegularFile(whitelistFile)) {
            cached = Collections.emptySet();
            return;
        }
        try {
            String json = new String(Files.readAllBytes(whitelistFile), StandardCharsets.UTF_8);
            JsonArray arr = GSON.fromJson(json, JsonArray.class);
            Set<UUID> set = new HashSet<>();
            if (arr != null) {
                for (JsonElement el : arr) {
                    if (!el.isJsonObject()) {
                        continue;
                    }
                    JsonObject obj = el.getAsJsonObject();
                    if (obj.has("uuid")) {
                        set.add(UUID.fromString(obj.get("uuid").getAsString()));
                    }
                }
            }
            cached = Collections.unmodifiableSet(set);
            log.info("Loaded {} whitelist UUIDs from {}", cached.size(), whitelistFile);
        } catch (Exception e) {
            log.warn("Failed to read whitelist.json: {}", e.getMessage());
            cached = Collections.emptySet();
        }
    }

    public boolean isWhitelisted(UUID uuid) {
        if (uuid == null) {
            return false;
        }
        return cached.contains(uuid);
    }

    public Set<UUID> all() {
        return cached;
    }
}
