package com.vesta.loader;

import java.nio.file.Path;

public class VestaMod {

    public static final String MOD_ID = "vesta-sync";
    public static final String MOD_NAME = "Vesta Server Sync";

    public void onInitialize() {
        ModLog.info("{} v{} initializing...", MOD_NAME, SyncStatus.PROTOCOL_VERSION);
    }

    public void onServerStart(
            Path serverRoot,
            String modpackName,
            String modpackVersion,
            String mcVersion,
            String loaderType,
            String loaderVersion,
            String host,
            int port
    ) {
        VestaSyncManager.get().initialize(
                serverRoot,
                modpackName,
                modpackVersion,
                mcVersion,
                loaderType,
                loaderVersion,
                host,
                port
        );
    }

    public void onServerStop() {
        VestaSyncManager.get().shutdown();
    }
}
