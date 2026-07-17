package com.vesta.sync.manifest;

import com.vesta.sync.sync.SyncCheckResult;
import com.vesta.sync.sync.SyncCheckService;

import java.io.IOException;
import java.nio.file.Path;
import java.util.ArrayList;
import java.util.Collections;
import java.util.List;
import java.util.Map;

public final class ModpackState {

    private final ServerModpackManifest manifest;
    private final List<PrivateFileEntry> privateFiles;

    public ModpackState(ServerModpackManifest manifest, List<PrivateFileEntry> privateFiles) {
        this.manifest = manifest;
        this.privateFiles = privateFiles != null
                ? Collections.unmodifiableList(new ArrayList<>(privateFiles))
                : Collections.emptyList();
    }

    public static ModpackState fromScan(
            Path scanRoot,
            List<String> scanPaths,
            KnownSourceRegistry known,
            UnresolvedPolicy unresolvedPolicy,
            List<String> extraPrivatePaths,
            String name,
            String version,
            String minecraftVersion,
            ModloaderInfo modloader,
            ServerInfo serverInfo
    ) throws IOException {
        ManifestBuilder.ScanResult scan = ManifestBuilder.scan(
                scanRoot, scanPaths, known, unresolvedPolicy, extraPrivatePaths);

        ServerModpackManifest manifest = new ServerModpackManifest();
        manifest.mode = ModpackMode.CUSTOM;
        manifest.name = name;
        manifest.version = version;
        manifest.minecraftVersion = minecraftVersion;
        manifest.modloader = modloader;
        manifest.mods = scan.mods;
        manifest.overrides = scan.overrides;
        manifest.serverInfo = serverInfo;
        return new ModpackState(manifest, scan.privateFiles);
    }

    public static ModpackState fromPlatform(
            String source,
            String projectId,
            String versionId,
            String downloadUrl,
            String name,
            String version,
            String minecraftVersion,
            ModloaderInfo modloader,
            ServerInfo serverInfo
    ) {
        ServerModpackManifest manifest = new ServerModpackManifest();
        manifest.mode = ModpackMode.PLATFORM;
        manifest.source = source;
        manifest.modpackId = projectId;
        manifest.versionId = versionId;
        manifest.downloadUrl = downloadUrl;
        manifest.name = name;
        manifest.version = version;
        manifest.minecraftVersion = minecraftVersion;
        manifest.modloader = modloader;
        manifest.serverInfo = serverInfo;
        return new ModpackState(manifest, Collections.emptyList());
    }

    public ServerModpackManifest getManifest() {
        return manifest;
    }

    public List<PrivateFileEntry> getPrivateFiles() {
        return privateFiles;
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
