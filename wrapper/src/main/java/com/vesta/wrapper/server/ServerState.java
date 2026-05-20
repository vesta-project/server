package com.vesta.wrapper.server;

import com.vesta.sync.manifest.*;
import com.vesta.wrapper.config.WrapperConfig;

public final class ServerState {

    private final ServerModpackManifest manifest;
    private volatile String currentVersion;

    public ServerState(WrapperConfig config) {
        this.currentVersion = config.modpackVersion;
        this.manifest = buildManifest(config);
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

    private static ServerModpackManifest buildManifest(WrapperConfig config) {
        ServerModpackManifest m = new ServerModpackManifest();
        m.mode = ModpackMode.CUSTOM;
        m.name = config.modpackName;
        m.version = config.modpackVersion;
        m.minecraftVersion = config.minecraftVersion;

        ModloaderInfo ml = new ModloaderInfo();
        ml.type = config.modloaderType;
        ml.version = config.modloaderVersion;
        m.modloader = ml;

        if (config.serverHost != null && !config.serverHost.isBlank()) {
            ServerInfo si = new ServerInfo();
            si.host = config.serverHost;
            si.port = config.serverPort;
            si.description = config.serverDescription;
            si.isRequired = config.serverRequired;
            m.serverInfo = si;
        }

        return m;
    }
}
