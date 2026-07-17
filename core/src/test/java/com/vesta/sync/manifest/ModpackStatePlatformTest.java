package com.vesta.sync.manifest;

import org.junit.jupiter.api.Test;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertTrue;

class ModpackStatePlatformTest {
    @Test
    void createsAPlatformReferenceWithoutHostedFiles() {
        ModloaderInfo loader = new ModloaderInfo();
        loader.type = "fabric";
        loader.version = "0.16.0";

        ModpackState state = ModpackState.fromPlatform(
                "modrinth",
                "project",
                "version",
                "",
                "Pack",
                "1.0.0",
                "1.21.1",
                loader,
                null
        );

        ServerModpackManifest manifest = state.getManifest();
        assertEquals(ModpackMode.PLATFORM, manifest.mode);
        assertEquals("modrinth", manifest.source);
        assertEquals("project", manifest.modpackId);
        assertEquals("version", manifest.versionId);
        assertTrue(manifest.mods.isEmpty());
        assertTrue(manifest.overrides.isEmpty());
    }
}
