package com.vesta.wrapper.server;

import com.vesta.sync.manifest.ServerModpackManifest;
import com.vesta.sync.manifest.ModloaderInfo;
import com.vesta.sync.manifest.ModpackMode;

public final class ServerState {

    private final ServerModpackManifest manifest;
    private volatile String currentVersion;

    public ServerState() {
        this.currentVersion = "1.0.0";
        this.manifest = new ServerModpackManifest();
        manifest.mode = ModpackMode.CUSTOM;
        manifest.name = "Demo Server Pack";
        manifest.version = currentVersion;
        manifest.minecraftVersion = "1.21.4";
        ModloaderInfo ml = new ModloaderInfo();
        ml.type = "fabric";
        ml.version = "0.18.4";
        manifest.modloader = ml;
    }

    public String getCurrentVersion() {
        return currentVersion;
    }

    public void setCurrentVersion(String version) {
        this.currentVersion = version;
        this.manifest.version = version;
    }

    public ServerModpackManifest getManifest() {
        return manifest;
    }
}
