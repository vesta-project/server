package com.vesta.sync.sync;

import com.vesta.sync.manifest.FileSource;
import com.vesta.sync.manifest.ManifestFile;
import com.vesta.sync.manifest.ModpackMode;
import com.vesta.sync.manifest.ServerModpackManifest;
import org.junit.jupiter.api.Test;

import java.util.Collections;
import java.util.HashMap;
import java.util.Map;

import static org.junit.jupiter.api.Assertions.*;

class SyncCheckServiceTest {

    @Test
    void noUpdateWhenVersionAndHashesMatch() {
        ServerModpackManifest manifest = manifest("1.0.0", file("mods/test.jar", "abc"));
        Map<String, String> installed = new HashMap<>();
        installed.put("mods/test.jar", "abc");
        SyncCheckResult result = SyncCheckService.check("1.0.0", installed, manifest);

        assertFalse(result.needsUpdate);
        assertTrue(result.actions.isEmpty());
        assertNull(result.manifest);
    }

    @Test
    void updateWhenVersionDiffers() {
        ServerModpackManifest manifest = manifest("1.1.0", file("mods/test.jar", "abc"));
        Map<String, String> installed = new HashMap<>();
        installed.put("mods/test.jar", "abc");
        SyncCheckResult result = SyncCheckService.check("1.0.0", installed, manifest);

        assertTrue(result.needsUpdate);
        assertEquals("1.1.0", result.newVersion);
        assertNotNull(result.manifest);
        assertFalse(result.actions.isEmpty());
    }

    @Test
    void updateWhenHashesDriftOnSameVersion() {
        ServerModpackManifest manifest = manifest("1.0.0", file("mods/test.jar", "abc"));
        Map<String, String> installed = new HashMap<>();
        installed.put("mods/test.jar", "wrong");
        SyncCheckResult result = SyncCheckService.check("1.0.0", installed, manifest);

        assertTrue(result.needsUpdate);
        assertEquals("update", result.actions.get(0).op);
    }

    private static ServerModpackManifest manifest(String version, ManifestFile file) {
        ServerModpackManifest manifest = new ServerModpackManifest();
        manifest.mode = ModpackMode.CUSTOM;
        manifest.version = version;
        manifest.mods = Collections.singletonList(file);
        return manifest;
    }

    private static ManifestFile file(String path, String sha256) {
        ManifestFile file = new ManifestFile();
        file.path = path;
        file.sha256 = sha256;
        file.size = 1;
        FileSource source = new FileSource();
        source.type = "server";
        file.source = source;
        return file;
    }
}
