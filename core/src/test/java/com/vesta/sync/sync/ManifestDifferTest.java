package com.vesta.sync.sync;

import com.vesta.sync.manifest.FileSource;
import com.vesta.sync.manifest.ManifestFile;
import com.vesta.sync.manifest.ModpackMode;
import com.vesta.sync.manifest.ServerModpackManifest;
import com.vesta.sync.manifest.SyncAction;
import org.junit.jupiter.api.Test;

import java.util.ArrayList;
import java.util.HashMap;
import java.util.List;
import java.util.Map;

import static org.junit.jupiter.api.Assertions.*;

class ManifestDifferTest {

    @Test
    void diffDetectsAddUpdateRemoveAndKeep() {
        ServerModpackManifest manifest = manifest(
                file("mods/keep.jar", "aaa"),
                file("mods/new.jar", "bbb"),
                file("mods/changed.jar", "ccc"),
                file("config/options.json", "ddd")
        );

        Map<String, String> installed = new HashMap<>();
        installed.put("mods/keep.jar", "aaa");
        installed.put("mods/changed.jar", "old-hash");
        installed.put("mods/old.jar", "eee");

        List<SyncAction> actions = ManifestDiffer.diff(manifest, installed);

        assertEquals(5, actions.size());
        assertAction(actions, "add", "mods/new.jar");
        assertAction(actions, "update", "mods/changed.jar");
        assertAction(actions, "keep", "mods/keep.jar");
        assertAction(actions, "remove", "mods/old.jar");
        assertAction(actions, "add", "config/options.json");
    }

    @Test
    void diffTreatsHashesCaseInsensitively() {
        ServerModpackManifest manifest = manifest(file("mods/test.jar", "AbCdEf"));
        Map<String, String> installed = new HashMap<>();
        installed.put("mods/test.jar", "abcdef");
        List<SyncAction> actions = ManifestDiffer.diff(manifest, installed);
        assertEquals(1, actions.size());
        assertEquals("keep", actions.get(0).op);
    }

    private static void assertAction(List<SyncAction> actions, String op, String path) {
        assertTrue(actions.stream().anyMatch(a -> op.equals(a.op) && path.equals(a.path)),
                "Expected action " + op + " for " + path);
    }

    private static ServerModpackManifest manifest(ManifestFile... files) {
        ServerModpackManifest manifest = new ServerModpackManifest();
        manifest.mode = ModpackMode.CUSTOM;
        manifest.version = "1.0.0";
        for (ManifestFile file : files) {
            if (file.path.startsWith("mods/")) {
                manifest.mods = append(manifest.mods, file);
            } else {
                manifest.overrides = append(manifest.overrides, file);
            }
        }
        return manifest;
    }

    private static List<ManifestFile> append(List<ManifestFile> list, ManifestFile file) {
        if (list == null || list.isEmpty()) {
            return java.util.Collections.singletonList(file);
        }
        ArrayList<ManifestFile> copy = new ArrayList<>(list);
        copy.add(file);
        return copy;
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
