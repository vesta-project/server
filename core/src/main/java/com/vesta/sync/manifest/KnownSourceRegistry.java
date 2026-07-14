package com.vesta.sync.manifest;

import com.google.gson.Gson;
import com.google.gson.reflect.TypeToken;
import org.slf4j.Logger;
import org.slf4j.LoggerFactory;

import java.io.IOException;
import java.io.Reader;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.Collections;
import java.util.HashMap;
import java.util.Map;

/**
 * Sidecar {@code .vesta-known.json} mapping relative paths to platform {@link FileSource} entries.
 * Populated by the launcher when seeding; wrapper reads on rescan.
 */
public final class KnownSourceRegistry {

    private static final Logger log = LoggerFactory.getLogger(KnownSourceRegistry.class);
    private static final Gson GSON = new Gson();
    public static final String FILENAME = ".vesta-known.json";

    private final Map<String, FileSource> byPath;

    public KnownSourceRegistry(Map<String, FileSource> byPath) {
        this.byPath = Collections.unmodifiableMap(new HashMap<>(byPath));
    }

    public static KnownSourceRegistry empty() {
        return new KnownSourceRegistry(Collections.emptyMap());
    }

    public static KnownSourceRegistry load(Path scanRoot) {
        Path file = scanRoot.resolve(FILENAME);
        if (!Files.isRegularFile(file)) {
            return empty();
        }
        try (Reader reader = Files.newBufferedReader(file, StandardCharsets.UTF_8)) {
            Map<String, FileSource> map = GSON.fromJson(reader, new TypeToken<Map<String, FileSource>>() {}.getType());
            if (map == null) {
                return empty();
            }
            log.info("Loaded {} known source entries from {}", map.size(), file);
            return new KnownSourceRegistry(map);
        } catch (IOException e) {
            log.warn("Failed to load {}: {}", file, e.getMessage());
            return empty();
        }
    }

    public FileSource get(String relativePath) {
        return byPath.get(relativePath);
    }

    public boolean isKnownPlatform(String relativePath) {
        FileSource src = byPath.get(relativePath);
        if (src == null || src.type == null) {
            return false;
        }
        String type = src.type.toLowerCase();
        return "modrinth".equals(type) || "curseforge".equals(type);
    }

    public Map<String, FileSource> all() {
        return byPath;
    }
}
