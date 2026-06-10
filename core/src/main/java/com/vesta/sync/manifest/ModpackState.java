package com.vesta.sync.manifest;

import com.vesta.sync.FileStore;
import com.vesta.sync.sync.SyncCheckResult;
import com.vesta.sync.sync.SyncCheckService;

import java.io.IOException;
import java.nio.file.Path;
import java.util.List;
import java.util.Map;

public final class ModpackState {

    private final ServerModpackManifest manifest;

    public ModpackState(ServerModpackManifest manifest) {
        this.manifest = manifest;
    }

    public static ModpackState fromScan(
            Path scanRoot,
            List<String> scanPaths,
            FileStore store,
            String name,
            String version,
            String minecraftVersion,
            ModloaderInfo modloader,
            ServerInfo serverInfo
    ) throws IOException {
        ManifestBuilder.ScanResult scan = ManifestBuilder.scan(scanRoot, scanPaths, store);

        ServerModpackManifest manifest = new ServerModpackManifest();
        manifest.mode = ModpackMode.CUSTOM;
        manifest.name = name;
        manifest.version = version;
        manifest.minecraftVersion = minecraftVersion;
        manifest.modloader = modloader;
        manifest.mods = scan.mods;
        manifest.overrides = scan.overrides;
        manifest.serverInfo = serverInfo;
        return new ModpackState(manifest);
    }

    public ServerModpackManifest getManifest() {
        return manifest;
    }

    public String getCurrentVersion() {
        return manifest.version;
    }

    public void setCurrentVersion(String version) {
        manifest.version = version;
    }

    public SyncCheckResult syncCheck(String clientVersion, Map<String, String> installedHashes) {
        return SyncCheckService.check(clientVersion, installedHashes, manifest);
    }
}
