package com.vesta.wrapper.server;

import com.vesta.sync.FileStore;
import com.vesta.sync.manifest.ModloaderInfo;
import com.vesta.sync.manifest.ModpackState;
import com.vesta.sync.manifest.ServerInfo;
import com.vesta.sync.manifest.ServerModpackManifest;
import com.vesta.sync.sync.SyncCheckResult;
import com.vesta.wrapper.config.WrapperConfig;
import org.slf4j.Logger;
import org.slf4j.LoggerFactory;

import java.util.Map;

public final class ServerState {

    private static final Logger log = LoggerFactory.getLogger(ServerState.class);

    private final ModpackState modpackState;
    private final String modpackDescription;

    public ServerState(WrapperConfig config, FileStore store) {
        this.modpackDescription = config.modpackDescription;
        ModloaderInfo modloader = new ModloaderInfo();
        modloader.type = config.modloaderType;
        modloader.version = config.modloaderVersion;

        ServerInfo serverInfo = null;
        if (config.serverHost != null && !config.serverHost.isBlank()) {
            serverInfo = new ServerInfo();
            serverInfo.host = config.serverHost;
            serverInfo.port = config.serverPort;
            serverInfo.description = config.serverDescription;
            serverInfo.isRequired = config.serverRequired;
        }

        try {
            this.modpackState = ModpackState.fromScan(
                    config.scanRoot,
                    config.scanPaths,
                    store,
                    config.modpackName,
                    config.modpackVersion,
                    config.minecraftVersion,
                    modloader,
                    serverInfo
            );
            log.info("Loaded modpack manifest: {} files ({} mods, {} overrides)",
                    modpackState.getManifest().mods.size() + modpackState.getManifest().overrides.size(),
                    modpackState.getManifest().mods.size(),
                    modpackState.getManifest().overrides.size());
        } catch (Exception e) {
            log.error("Failed to scan modpack from {}: {}", config.scanRoot, e.getMessage(), e);
            throw new IllegalStateException("Failed to build modpack manifest", e);
        }
    }

    public String getCurrentVersion() {
        return modpackState.getCurrentVersion();
    }

    public void setCurrentVersion(String version) {
        modpackState.setCurrentVersion(version);
    }

    public ServerModpackManifest getManifest() {
        return modpackState.getManifest();
    }

    public SyncCheckResult syncCheck(String clientVersion, Map<String, String> installedHashes) {
        return modpackState.syncCheck(clientVersion, installedHashes);
    }

    public String getModpackDescription() {
        return modpackDescription;
    }
}
