package com.vesta.sync.sync;

import com.vesta.sync.manifest.ManifestFile;
import com.vesta.sync.manifest.ServerModpackManifest;
import com.vesta.sync.manifest.SyncAction;
import com.vesta.sync.util.PathSanitizer;

import java.util.ArrayList;
import java.util.Collections;
import java.util.HashMap;
import java.util.HashSet;
import java.util.List;
import java.util.Locale;
import java.util.Map;
import java.util.Set;

public final class ManifestDiffer {

    private ManifestDiffer() {}

    public static List<SyncAction> diff(ServerModpackManifest manifest, Map<String, String> installedHashes) {
        if (manifest == null) {
            return Collections.emptyList();
        }

        Map<String, ManifestFile> desired = new HashMap<>();
        for (ManifestFile file : manifest.mods) {
            desired.put(normalizePath(file.path), file);
        }
        for (ManifestFile file : manifest.overrides) {
            desired.put(normalizePath(file.path), file);
        }

        Map<String, String> installed = normalizeInstalled(installedHashes);
        List<SyncAction> actions = new ArrayList<>();
        Set<String> seen = new HashSet<>();

        for (Map.Entry<String, ManifestFile> entry : desired.entrySet()) {
            String path = entry.getKey();
            ManifestFile file = entry.getValue();
            seen.add(path);

            String installedHash = installed.get(path);
            if (installedHash == null) {
                actions.add(action("add", file));
            } else if (!hashesEqual(installedHash, file.sha256)) {
                actions.add(action("update", file));
            } else {
                actions.add(action("keep", file));
            }
        }

        for (Map.Entry<String, String> entry : installed.entrySet()) {
            if (!seen.contains(entry.getKey())) {
                SyncAction remove = new SyncAction();
                remove.op = "remove";
                remove.path = entry.getKey();
                remove.sha256 = entry.getValue();
                actions.add(remove);
            }
        }

        actions.sort((a, b) -> a.path.compareTo(b.path));
        return actions;
    }

    private static SyncAction action(String op, ManifestFile file) {
        SyncAction action = new SyncAction();
        action.op = op;
        action.path = file.path;
        action.sha256 = file.sha256;
        action.source = file.source;
        return action;
    }

    private static Map<String, String> normalizeInstalled(Map<String, String> installedHashes) {
        if (installedHashes == null || installedHashes.isEmpty()) {
            return Collections.emptyMap();
        }

        Map<String, String> normalized = new HashMap<>();
        for (Map.Entry<String, String> entry : installedHashes.entrySet()) {
            if (entry.getKey() == null || entry.getKey().trim().isEmpty()) {
                continue;
            }
            normalized.put(normalizePath(entry.getKey()), entry.getValue());
        }
        return normalized;
    }

    private static String normalizePath(String path) {
        return PathSanitizer.sanitize(path.replace('\\', '/'));
    }

    private static boolean hashesEqual(String left, String right) {
        if (left == null || right == null) {
            return false;
        }
        return left.toLowerCase(Locale.ROOT).equals(right.toLowerCase(Locale.ROOT));
    }
}
