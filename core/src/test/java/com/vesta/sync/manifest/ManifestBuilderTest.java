package com.vesta.sync.manifest;

import com.vesta.sync.FileStore;
import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.io.TempDir;

import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.Arrays;
import java.util.Collections;
import java.util.List;

import static org.junit.jupiter.api.Assertions.*;

class ManifestBuilderTest {

    @TempDir
    Path tempDir;

    @Test
    void scanCollectsModsAndOverrides() throws Exception {
        Path modsDir = tempDir.resolve("mods");
        Path configDir = tempDir.resolve("config");
        Files.createDirectories(modsDir);
        Files.createDirectories(configDir);

        Files.write(modsDir.resolve("test-mod.jar"), "mod-content".getBytes(StandardCharsets.UTF_8));
        Files.write(configDir.resolve("options.json"), "{\"enabled\":true}".getBytes(StandardCharsets.UTF_8));

        FileStore store = new FileStore(tempDir.resolve("store"));
        ManifestBuilder.ScanResult result = ManifestBuilder.scan(
                tempDir, Arrays.asList("mods", "config"), store);

        assertEquals(1, result.mods.size());
        assertEquals(1, result.overrides.size());
        assertEquals("mods/test-mod.jar", result.mods.get(0).path);
        assertEquals("config/options.json", result.overrides.get(0).path);
        assertNotNull(result.mods.get(0).sha256);
        assertEquals("server", result.mods.get(0).source.type);
        assertTrue(store.exists(result.mods.get(0).sha256));
    }

    @Test
    void scanSkipsMissingDirectories() throws Exception {
        FileStore store = new FileStore(tempDir.resolve("store"));
        ManifestBuilder.ScanResult result = ManifestBuilder.scan(
                tempDir, Arrays.asList("mods", "config"), store);
        assertTrue(result.mods.isEmpty());
        assertTrue(result.overrides.isEmpty());
    }

    @Test
    void scanSkipsHiddenFiles() throws Exception {
        Path modsDir = tempDir.resolve("mods");
        Files.createDirectories(modsDir);
        Files.write(modsDir.resolve(".hidden.jar"), "hidden".getBytes(StandardCharsets.UTF_8));
        Files.write(modsDir.resolve("visible.jar"), "visible".getBytes(StandardCharsets.UTF_8));

        FileStore store = new FileStore(tempDir.resolve("store"));
        ManifestBuilder.ScanResult result = ManifestBuilder.scan(
                tempDir, Collections.singletonList("mods"), store);

        assertEquals(1, result.mods.size());
        assertEquals("mods/visible.jar", result.mods.get(0).path);
    }
}
