package com.vesta.wrapper.server;

import com.vesta.sync.VestaSync;
import com.vesta.sync.manifest.KnownSourceRegistry;
import com.vesta.sync.manifest.LocalArchivePack;
import com.vesta.sync.manifest.ModloaderInfo;
import com.vesta.sync.manifest.ModpackMode;
import com.vesta.sync.manifest.ModpackState;
import com.vesta.sync.manifest.PackSource;
import com.vesta.sync.manifest.PrivateFileEntry;
import com.vesta.sync.manifest.ServerInfo;
import com.vesta.sync.manifest.ServerModpackManifest;
import com.vesta.sync.manifest.VestaPackBuilder;
import com.vesta.sync.protocol.PushUpdateAvailable;
import com.vesta.sync.sync.SyncCheckResult;
import com.vesta.wrapper.config.WrapperConfig;
import com.vesta.wrapper.content.ContentInstaller;
import org.slf4j.Logger;
import org.slf4j.LoggerFactory;

import java.io.IOException;
import java.io.InputStream;
import java.nio.file.Files;
import java.nio.file.Path;
import java.time.Instant;
import java.util.ArrayList;
import java.util.List;
import java.util.Map;
import java.util.Optional;
import java.util.concurrent.CopyOnWriteArrayList;
import java.util.function.Consumer;

public final class ServerState {

    private static final Logger log = LoggerFactory.getLogger(ServerState.class);

    private final WrapperConfig config;
    private final ContentInstaller installer;
    private final String modpackDescription;
    private final List<Consumer<PushUpdateAvailable>> pushListeners = new CopyOnWriteArrayList<>();

    private volatile ModpackState modpackState;
    private volatile LocalArchivePack localArchive;
    private volatile Instant updatedAt;

    public ServerState(WrapperConfig config) {
        this.config = config;
        this.installer = new ContentInstaller(config.scanRoot);
        this.modpackDescription = config.modpackDescription;
        this.updatedAt = Instant.now();
        rescan();
    }

    public synchronized void rescan() {
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
            String previous = modpackState != null ? modpackState.getCurrentVersion() : null;
            LocalArchivePack nextArchive = null;
            PackSource source = config.modpackSource;

            if (source.isPlatform()) {
                validatePlatformConfig(source);
                this.modpackState = ModpackState.fromPlatform(
                        source.wireName(),
                        config.modpackPlatformProjectId,
                        config.modpackPlatformVersionId,
                        config.modpackPlatformDownloadUrl,
                        config.modpackName,
                        config.modpackVersion,
                        config.minecraftVersion,
                        modloader,
                        serverInfo
                );
            } else if (source == PackSource.ARCHIVE) {
                nextArchive = LocalArchivePack.resolve(config.scanRoot, config.modpackArchivePath);
                this.modpackState = ModpackState.fromArchive(
                        nextArchive,
                        VestaSync.API_MODPACK_ARCHIVE,
                        config.modpackName,
                        config.modpackVersion,
                        config.minecraftVersion,
                        modloader,
                        serverInfo
                );
            } else {
                KnownSourceRegistry known = KnownSourceRegistry.load(config.scanRoot);
                this.modpackState = ModpackState.fromScan(
                        config.scanRoot,
                        config.scanPaths,
                        known,
                        config.unresolvedPolicy,
                        config.privatePaths,
                        config.modpackName,
                        config.modpackVersion,
                        config.minecraftVersion,
                        modloader,
                        serverInfo
                );
            }
            this.updatedAt = Instant.now();
            ServerModpackManifest m = modpackState.getManifest();
            m.revision = config.modpackRevision;
            m.minimumRevision = config.minimumRevision;

            if (source == PackSource.SELF_HOSTED) {
                Path packPath = config.scanRoot.resolve(".vesta").resolve("pack.vestapack");
                try {
                    nextArchive = VestaPackBuilder.build(config.scanRoot, m, packPath);
                    m.downloadUrl = VestaSync.API_MODPACK_ARCHIVE;
                    m.archivePath = ".vesta/pack.vestapack";
                    m.archiveSha256 = nextArchive.sha256;
                    m.archiveSize = nextArchive.size;
                    m.archiveFormat = VestaPackBuilder.FORMAT;
                } catch (Exception packError) {
                    log.error("Failed to generate Vesta pack: {}", packError.getMessage(), packError);
                    // Keep the previous generated pack available if present.
                    if (this.localArchive != null
                            && VestaPackBuilder.FORMAT.equals(this.localArchive.format)
                            && Files.exists(this.localArchive.resolvedPath)) {
                        nextArchive = this.localArchive;
                        m.downloadUrl = VestaSync.API_MODPACK_ARCHIVE;
                        m.archivePath = ".vesta/pack.vestapack";
                        m.archiveSha256 = nextArchive.sha256;
                        m.archiveSize = nextArchive.size;
                        m.archiveFormat = nextArchive.format;
                    }
                }
            }

            this.localArchive = nextArchive;
            if (m.mode == ModpackMode.PLATFORM) {
                log.info("Loaded {} platform pack reference: {} @ {}",
                        m.source, m.modpackId, m.versionId);
            } else if (m.mode == ModpackMode.ARCHIVE) {
                log.info("Loaded local archive pack: {} ({} bytes, {})",
                        m.archivePath, m.archiveSize, m.archiveFormat);
            } else {
                log.info("Loaded modpack manifest: {} public files ({} private){}",
                        m.mods.size() + m.overrides.size(),
                        modpackState.getPrivateFiles().size(),
                        nextArchive != null
                                ? (", Vesta pack " + nextArchive.sha256.substring(0, 12) + "…")
                                : "");
            }
            if (previous != null) {
                notifyPush(previous);
            }
        } catch (Exception e) {
            log.error("Failed to scan modpack from {}: {}", config.scanRoot, e.getMessage(), e);
            if (modpackState == null) {
                throw new IllegalStateException("Failed to build modpack manifest", e);
            }
        }
    }

    private void validatePlatformConfig(PackSource source) {
        if (config.modpackPlatformProjectId.isBlank()
                || config.modpackPlatformVersionId.isBlank()) {
            throw new IllegalArgumentException(
                    "modpack.source = \"" + source.wireName()
                            + "\" requires modpack.platform.project-id and version-id"
            );
        }
        if (!config.modpackArchivePath.isBlank()) {
            throw new IllegalArgumentException(
                    "modpack.archive.path is only valid when modpack.source = \"archive\""
            );
        }
    }

    public ContentInstaller installer() {
        return installer;
    }

    public Optional<Path> localArchivePath() {
        return localArchive == null ? Optional.empty() : Optional.of(localArchive.resolvedPath);
    }

    public Optional<LocalArchivePack> localArchive() {
        return Optional.ofNullable(localArchive);
    }

    public ContentInstaller.Result install(String relativePath, InputStream data) throws IOException {
        ContentInstaller.Result result = installer.install(relativePath, data);
        rescan();
        return result;
    }

    public boolean remove(String relativePath) throws IOException {
        boolean deleted = installer.remove(relativePath);
        if (deleted) {
            rescan();
        }
        return deleted;
    }

    public List<PrivateFileEntry> getPrivateFiles() {
        return modpackState.getPrivateFiles();
    }

    public void addPushListener(Consumer<PushUpdateAvailable> listener) {
        pushListeners.add(listener);
    }

    public void removePushListener(Consumer<PushUpdateAvailable> listener) {
        pushListeners.remove(listener);
    }

    private void notifyPush(String previousVersion) {
        PushUpdateAvailable push = new PushUpdateAvailable();
        push.ts = updatedAt.toString();
        push.session = "";
        push.seq = 0;
        push.newVersion = getCurrentVersion();
        push.changelog = "Manifest rescanned";
        push.requiresRestart = true;
        for (Consumer<PushUpdateAvailable> listener : pushListeners) {
            try {
                listener.accept(push);
            } catch (Exception e) {
                log.warn("Push listener failed: {}", e.getMessage());
            }
        }
    }

    public Instant getUpdatedAt() {
        return updatedAt;
    }

    public String getCurrentVersion() {
        return modpackState.getCurrentVersion();
    }

    public long getCurrentRevision() {
        return config.modpackRevision;
    }

    public long getMinimumRevision() {
        return config.minimumRevision;
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

    public List<Map<String, String>> listVersions() {
        List<Map<String, String>> versions = new ArrayList<>();
        versions.add(Map.of(
                "version", getCurrentVersion(),
                "published_at", updatedAt.toString()
        ));
        return versions;
    }
}
