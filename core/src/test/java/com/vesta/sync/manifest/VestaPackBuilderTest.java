package com.vesta.sync.manifest;

import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.io.TempDir;

import java.io.InputStreamReader;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.Collections;
import java.util.zip.ZipEntry;
import java.util.zip.ZipFile;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertTrue;

class VestaPackBuilderTest {

    @TempDir
    Path temp;

    @Test
    void embedsUnknownFilesAndLeavesKnownSourcesExternal() throws Exception {
        Path mods = temp.resolve("mods");
        Files.createDirectories(mods);
        Files.write(mods.resolve("known.jar"), "known".getBytes(StandardCharsets.UTF_8));
        Files.write(mods.resolve("privateish.jar"), "embedded".getBytes(StandardCharsets.UTF_8));

        ManifestFile known = new ManifestFile();
        known.path = "mods/known.jar";
        known.sha256 = "aa";
        known.sha1 = "bb";
        known.size = 5;
        known.source = new FileSource();
        known.source.type = "modrinth";
        known.source.platform = "modrinth";
        known.source.projectId = "proj";
        known.source.versionId = "ver";
        known.source.url = "https://cdn.example/known.jar";

        ManifestFile embedded = new ManifestFile();
        embedded.path = "mods/privateish.jar";
        embedded.sha256 = "cc";
        embedded.size = 8;
        embedded.source = new FileSource();
        embedded.source.type = "server";

        ServerModpackManifest manifest = new ServerModpackManifest();
        manifest.name = "Hybrid";
        manifest.version = "1.2.3";
        manifest.revision = 4;
        manifest.minimumRevision = 2;
        manifest.minecraftVersion = "1.21.1";
        manifest.modloader = new ModloaderInfo();
        manifest.modloader.type = "fabric";
        manifest.modloader.version = "0.16.0";
        manifest.mods = java.util.Arrays.asList(known, embedded);
        manifest.overrides = Collections.emptyList();

        Path output = temp.resolve(".vesta").resolve("pack.vestapack");
        LocalArchivePack pack = VestaPackBuilder.build(temp, manifest, output);

        assertEquals(VestaPackBuilder.FORMAT, pack.format);
        assertTrue(Files.isRegularFile(pack.resolvedPath));

        try (ZipFile zip = new ZipFile(pack.resolvedPath.toFile())) {
            assertTrue(zip.getEntry(VestaPackBuilder.INDEX_NAME) != null);
            assertTrue(zip.getEntry("overrides/mods/privateish.jar") != null);
            assertFalse(zip.getEntry("overrides/mods/known.jar") != null);

            ZipEntry indexEntry = zip.getEntry(VestaPackBuilder.INDEX_NAME);
            VestaPackBuilder.Index index = new com.google.gson.Gson().fromJson(
                    new InputStreamReader(zip.getInputStream(indexEntry), StandardCharsets.UTF_8),
                    VestaPackBuilder.Index.class
            );
            assertEquals(1, index.formatVersion);
            assertEquals(2, index.files.size());
            assertEquals("mods/known.jar", index.files.get(0).path);
            assertEquals("modrinth", index.files.get(0).source.type);
            assertEquals("mods/privateish.jar", index.files.get(1).path);
            assertEquals("embedded", index.files.get(1).source.type);
        }
    }
}
