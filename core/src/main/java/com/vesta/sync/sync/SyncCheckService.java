package com.vesta.sync.sync;

import com.vesta.sync.manifest.ServerModpackManifest;
import com.vesta.sync.manifest.SyncAction;

import java.util.Collections;
import java.util.List;
import java.util.Map;
import java.util.Objects;

public final class SyncCheckService {

    private SyncCheckService() {}

    public static SyncCheckResult check(
            String clientVersion,
            Map<String, String> installedHashes,
            ServerModpackManifest manifest
    ) {
        String serverVersion = manifest == null ? "" : nullToEmpty(manifest.version);
        String currentVersion = nullToEmpty(clientVersion);

        List<SyncAction> actions = ManifestDiffer.diff(manifest, installedHashes);
        boolean versionMismatch = !Objects.equals(serverVersion, currentVersion);
        boolean fileChanges = actions.stream().anyMatch(action -> !"keep".equals(action.op));

        boolean needsUpdate = versionMismatch || fileChanges;
        ServerModpackManifest responseManifest = needsUpdate ? manifest : null;

        return new SyncCheckResult(
                needsUpdate,
                serverVersion,
                needsUpdate ? actions : Collections.<SyncAction>emptyList(),
                responseManifest
        );
    }

    private static String nullToEmpty(String value) {
        return value == null ? "" : value;
    }
}
