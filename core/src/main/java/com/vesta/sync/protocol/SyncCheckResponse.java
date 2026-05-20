package com.vesta.sync.protocol;

import com.vesta.sync.manifest.ServerModpackManifest;
import com.vesta.sync.manifest.SyncAction;

import java.util.Collections;
import java.util.List;

public final class SyncCheckResponse {
    public String type = "sync_check_response";
    public long seq;
    public String session;
    public String ts;
    public boolean needsUpdate;
    public String newVersion;
    public String changelog;
    public ServerModpackManifest manifest;
    public List<SyncAction> actions = Collections.emptyList();
}
