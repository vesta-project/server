package com.vesta.sync.sync;

import com.vesta.sync.manifest.ServerModpackManifest;
import com.vesta.sync.manifest.SyncAction;

import java.util.ArrayList;
import java.util.Collections;
import java.util.List;

public final class SyncCheckResult {

    public final boolean needsUpdate;
    public final String newVersion;
    public final List<SyncAction> actions;
    public final ServerModpackManifest manifest;

    public SyncCheckResult(
            boolean needsUpdate,
            String newVersion,
            List<SyncAction> actions,
            ServerModpackManifest manifest
    ) {
        this.needsUpdate = needsUpdate;
        this.newVersion = newVersion;
        this.actions = actions == null
                ? Collections.<SyncAction>emptyList()
                : Collections.unmodifiableList(new ArrayList<>(actions));
        this.manifest = manifest;
    }
}
