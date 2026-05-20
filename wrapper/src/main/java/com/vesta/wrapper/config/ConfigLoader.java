package com.vesta.wrapper.config;

import com.moandjiezana.toml.Toml;
import org.slf4j.Logger;
import org.slf4j.LoggerFactory;

import java.nio.file.Files;
import java.nio.file.Path;

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

            b.modpackName(toml.getString("modpack.name", b.modpackName));
            b.modpackVersion(toml.getString("modpack.version", b.modpackVersion));
            b.minecraftVersion(toml.getString("modpack.minecraft-version", b.minecraftVersion));

            b.modloaderType(toml.getString("modpack.modloader.type", b.modloaderType));
            b.modloaderVersion(toml.getString("modpack.modloader.version", b.modloaderVersion));

            b.serverHost(toml.getString("modpack.server.host", b.serverHost));
            b.serverPort(toml.getLong("modpack.server.port", (long) b.serverPort).intValue());
            b.serverDescription(toml.getString("modpack.server.description", b.serverDescription));
            b.serverRequired(toml.getBoolean("modpack.server.is-required", b.serverRequired));

            log.info("Loaded config from {}", file);
        } catch (Exception e) {
            log.error("Failed to parse config: {}, using defaults", e.getMessage());
        }

        return b.build();
    }

    private ConfigLoader() {}
}
