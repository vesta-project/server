package com.vesta.sync.manifest;

import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.io.TempDir;

import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertThrows;
import static org.junit.jupiter.api.Assertions.assertTrue;

class LocalArchivePackTest {

    @TempDir
    Path temp;

    @Test
    void resolvesRelativeArchiveUnderServerRoot() throws Exception {
        Path packs = temp.resolve("packs");
        Files.createDirectories(packs);
        Path archive = packs.resolve("demo.mrpack");
        Files.write(archive, "pack-bytes".getBytes(StandardCharsets.UTF_8));

        LocalArchivePack resolved = LocalArchivePack.resolve(temp, "packs/demo.mrpack");
        assertEquals("packs/demo.mrpack", resolved.relativePath);
        assertEquals("mrpack", resolved.format);
        assertEquals(Files.size(archive), resolved.size);
        assertEquals(64, resolved.sha256.length());
        assertTrue(Files.isSameFile(archive, resolved.resolvedPath));
    }

    @Test
    void rejectsPathEscape() throws Exception {
        Path outside = temp.resolve("outside.zip");
        Files.write(outside, "x".getBytes(StandardCharsets.UTF_8));
        Path root = temp.resolve("server");
        Files.createDirectories(root);

        assertThrows(
                IllegalArgumentException.class,
                () -> LocalArchivePack.resolve(root, "../outside.zip")
        );
    }

    @Test
    void rejectsUnsupportedExtension() throws Exception {
        Path file = temp.resolve("notes.txt");
        Files.write(file, "nope".getBytes(StandardCharsets.UTF_8));
        assertThrows(
                IllegalArgumentException.class,
                () -> LocalArchivePack.resolve(temp, "notes.txt")
        );
    }

    @Test
    void createsArchiveManifestWithoutExtractingFiles() throws Exception {
        Path archive = temp.resolve("pack.zip");
        Files.write(archive, "zip-bytes".getBytes(StandardCharsets.UTF_8));
        LocalArchivePack resolved = LocalArchivePack.resolve(temp, "pack.zip");

        ModloaderInfo loader = new ModloaderInfo();
        loader.type = "fabric";
        loader.version = "0.16.0";

        ModpackState state = ModpackState.fromArchive(
                resolved,
                "/api/v1/modpack/archive",
                "Local Pack",
                "2.0.0",
                "1.21.1",
                loader,
                null
        );

        ServerModpackManifest manifest = state.getManifest();
        assertEquals(ModpackMode.ARCHIVE, manifest.mode);
        assertEquals("archive", manifest.source);
        assertEquals("/api/v1/modpack/archive", manifest.downloadUrl);
        assertEquals("pack.zip", manifest.archivePath);
        assertEquals("zip", manifest.archiveFormat);
        assertTrue(manifest.mods.isEmpty());
        assertTrue(manifest.overrides.isEmpty());
    }
}
