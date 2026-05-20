package com.vesta.sync.protocol;

import java.util.Collections;
import java.util.Map;

public final class SyncCheck {
    public String type = "sync_check";
    public long seq;
    public String session;
    public String ts;
    public String mode;
    public String modpackId;
    public String source;
    public String currentVersion;
    public Map<String, String> installedHashes = Collections.emptyMap();
}
