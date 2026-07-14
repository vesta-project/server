package com.vesta.wrapper.config;

import com.moandjiezana.toml.Toml;
import com.vesta.sync.auth.SecurityMode;
import org.slf4j.Logger;
import org.slf4j.LoggerFactory;

import java.nio.file.Files;
import java.nio.file.Path;
import java.util.List;

public final class ConfigLoader {

    private static final Logger log = LoggerFactory.getLogger(ConfigLoader.class);

    public static WrapperConfig load(Path file) {
        WrapperConfig.Builder b = new WrapperConfig.Builder();

        if (!Files.exists(file)) {
            log.warn("Config file not found: {}, using defaults", file);
            return b.build();
        }

        try {
            Toml toml = new Toml().read(file.toFile());

            b.tcpPort(toml.getLong("server.tcp-port", (long) b.tcpPort).intValue());
            b.httpPort(toml.getLong("server.http-port", (long) b.httpPort).intValue());

            var store = toml.getString("server.file-store");
            if (store != null) b.fileStoreDir(Path.of(store));

            var scanRoot = toml.getString("server.scan-root");
            if (scanRoot != null) b.scanRoot(Path.of(scanRoot));

            List<String> scanPaths = toml.getList("server.scan-paths");
            if (scanPaths != null && !scanPaths.isEmpty()) {
                b.scanPaths(scanPaths);
            }

            b.publicScheme(toml.getString("server.public.scheme", b.publicScheme));
            b.publicHost(toml.getString("server.public.host", b.publicHost));
            b.publicPort(toml.getLong("server.public.port", (long) b.publicPort).intValue());

            b.modpackName(toml.getString("modpack.name", b.modpackName));
            b.modpackDescription(toml.getString("modpack.description", b.modpackDescription));
            b.modpackVersion(toml.getString("modpack.version", b.modpackVersion));
            b.minecraftVersion(toml.getString("modpack.minecraft-version", b.minecraftVersion));

            b.modloaderType(toml.getString("modpack.modloader.type", b.modloaderType));
            b.modloaderVersion(toml.getString("modpack.modloader.version", b.modloaderVersion));

            b.serverHost(toml.getString("modpack.server.host", b.serverHost));
            b.serverPort(toml.getLong("modpack.server.port", (long) b.serverPort).intValue());
            b.serverDescription(toml.getString("modpack.server.description", b.serverDescription));
            b.serverRequired(toml.getBoolean("modpack.server.is-required", b.serverRequired));

            String mode = toml.getString("security.mode", "open");
            b.securityMode(SecurityMode.parse(mode));
            b.securityPassword(toml.getString("security.password", b.securityPassword));
            b.tokenTtlSeconds(toml.getLong("security.token-ttl-seconds", b.tokenTtlSeconds));
            b.controlPort(toml.getLong("security.control-port", (long) b.controlPort).intValue());

            log.info("Loaded config from {}", file);
        } catch (Exception e) {
            log.error("Failed to parse config: {}, using defaults", e.getMessage());
        }

        return b.build();
    }

    private ConfigLoader() {}
}
