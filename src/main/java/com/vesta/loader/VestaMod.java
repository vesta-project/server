package com.vesta.loader;

import java.nio.file.Path;

public class VestaMod {

    public static final String MOD_ID = "vesta-sync";
    public static final String MOD_NAME = "Vesta Server Sync";

    public void onInitialize() {
        ModLog.info("{} v{} initializing...", MOD_NAME, SyncStatus.PROTOCOL_VERSION);
    }

    public void onServerStart(Path dataDir, String mcVersion, String loaderType, String host, int port) {
        VestaSyncManager.get().initialize(dataDir, mcVersion, loaderType, host, port);
    }

    public void onServerStop() {
        VestaSyncManager.get().shutdown();
    }
}
