package com.vesta.sync.manifest;

import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.io.TempDir;

import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.Arrays;
import java.util.Collections;
import java.util.HashMap;
import java.util.Map;

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

        ManifestBuilder.ScanResult result = ManifestBuilder.scan(
                tempDir,
                Arrays.asList("mods", "config"),
                KnownSourceRegistry.empty(),
                UnresolvedPolicy.SERVER,
                Collections.emptyList());

        assertEquals(1, result.mods.size());
        assertEquals(1, result.overrides.size());
        assertEquals("mods/test-mod.jar", result.mods.get(0).path);
        assertEquals("config/options.json", result.overrides.get(0).path);
        assertNotNull(result.mods.get(0).sha256);
        assertEquals("server", result.mods.get(0).source.type);
    }

    @Test
    void scanSkipsMissingDirectories() throws Exception {
        ManifestBuilder.ScanResult result = ManifestBuilder.scan(
                tempDir,
                Arrays.asList("mods", "config"),
                KnownSourceRegistry.empty(),
                UnresolvedPolicy.SERVER,
                Collections.emptyList());
        assertTrue(result.mods.isEmpty());
        assertTrue(result.overrides.isEmpty());
    }

    @Test
    void scanSkipsHiddenFiles() throws Exception {
        Path modsDir = tempDir.resolve("mods");
        Files.createDirectories(modsDir);
        Files.write(modsDir.resolve(".hidden.jar"), "hidden".getBytes(StandardCharsets.UTF_8));
        Files.write(modsDir.resolve("visible.jar"), "visible".getBytes(StandardCharsets.UTF_8));

        ManifestBuilder.ScanResult result = ManifestBuilder.scan(
                tempDir,
                Collections.singletonList("mods"),
                KnownSourceRegistry.empty(),
                UnresolvedPolicy.SERVER,
                Collections.emptyList());

        assertEquals(1, result.mods.size());
        assertEquals("mods/visible.jar", result.mods.get(0).path);
    }

    @Test
    void knownPlatformStaysPublic() throws Exception {
        Path modsDir = tempDir.resolve("mods");
        Files.createDirectories(modsDir);
        Files.write(modsDir.resolve("known.jar"), "known".getBytes(StandardCharsets.UTF_8));

        FileSource src = new FileSource();
        src.type = "modrinth";
        src.platform = "modrinth";
        src.projectId = "abc";
        src.versionId = "1";
        Map<String, FileSource> map = new HashMap<>();
        map.put("mods/known.jar", src);

        ManifestBuilder.ScanResult result = ManifestBuilder.scan(
                tempDir,
                Collections.singletonList("mods"),
                new KnownSourceRegistry(map),
                UnresolvedPolicy.PRIVATE,
                Collections.emptyList());

        assertEquals(1, result.mods.size());
        assertEquals("modrinth", result.mods.get(0).source.type);
        assertTrue(result.privateFiles.isEmpty());
    }

    @Test
    void unresolvedPrivatePolicyListsOnManifest() throws Exception {
        Path modsDir = tempDir.resolve("mods");
        Files.createDirectories(modsDir);
        Files.write(modsDir.resolve("manual.jar"), "manual".getBytes(StandardCharsets.UTF_8));

        ManifestBuilder.ScanResult result = ManifestBuilder.scan(
                tempDir,
                Collections.singletonList("mods"),
                KnownSourceRegistry.empty(),
                UnresolvedPolicy.PRIVATE,
                Collections.emptyList());

        assertEquals(1, result.mods.size());
        assertEquals("mods/manual.jar", result.mods.get(0).path);
        assertEquals("server", result.mods.get(0).source.type);
        assertTrue(result.privateFiles.isEmpty());
    }

    @Test
    void extraPrivatePathsAppearOnManifest() throws Exception {
        Path privateDir = tempDir.resolve("vesta-private");
        Files.createDirectories(privateDir);
        Files.write(privateDir.resolve("secret.jar"), "secret".getBytes(StandardCharsets.UTF_8));

        ManifestBuilder.ScanResult result = ManifestBuilder.scan(
                tempDir,
                Collections.emptyList(),
                KnownSourceRegistry.empty(),
                UnresolvedPolicy.SERVER,
                Collections.singletonList("vesta-private"));

        assertEquals(1, result.overrides.size());
        assertEquals("vesta-private/secret.jar", result.overrides.get(0).path);
    }
}
