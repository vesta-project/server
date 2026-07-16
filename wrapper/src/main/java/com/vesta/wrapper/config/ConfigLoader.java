package com.vesta.wrapper.config;

import com.moandjiezana.toml.Toml;
import com.vesta.sync.auth.CapabilityLevel;
import com.vesta.sync.auth.SecurityCapabilities;
import com.vesta.sync.auth.SecurityMode;
import com.vesta.sync.manifest.UnresolvedPolicy;
import org.slf4j.Logger;
import org.slf4j.LoggerFactory;

import java.nio.file.Files;
import java.nio.file.Path;
import java.util.ArrayList;
import java.util.HashSet;
import java.util.List;
import java.util.Set;
import java.util.UUID;

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
            b.tlsEnabled(toml.getBoolean("server.tls.enabled", b.tlsEnabled));
            String keyStore = toml.getString("server.tls.keystore");
            if (keyStore != null && !keyStore.isBlank()) {
                b.tlsKeyStore(Path.of(keyStore));
            }
            b.tlsKeyStorePassword(toml.getString(
                    "server.tls.keystore-password",
                    b.tlsKeyStorePassword
            ));
            b.tlsAutoGenerate(toml.getBoolean(
                    "server.tls.auto-generate",
                    b.tlsAutoGenerate
            ));

            var scanRoot = toml.getString("server.scan-root");
            if (scanRoot != null) b.scanRoot(Path.of(scanRoot));

            List<String> scanPaths = toml.getList("server.scan-paths");
            if (scanPaths != null && !scanPaths.isEmpty()) {
                b.scanPaths(scanPaths);
            }

            b.publicScheme(toml.getString("server.public.scheme", b.publicScheme));
            b.publicHost(toml.getString("server.public.host", b.publicHost));
            b.publicPort(toml.getLong("server.public.port", (long) b.publicPort).intValue());

            String configuredServerId = toml.getString("server.identity.id");
            if (configuredServerId != null && !configuredServerId.isBlank()) {
                b.serverId(UUID.fromString(configuredServerId.trim()));
            }
            b.serverDisplayName(toml.getString(
                    "server.identity.name",
                    b.serverDisplayName
            ));
            b.serverBannerUrl(toml.getString(
                    "server.identity.banner-url",
                    b.serverBannerUrl
            ));

            b.modpackName(toml.getString("modpack.name", b.modpackName));
            b.modpackDescription(toml.getString("modpack.description", b.modpackDescription));
            b.modpackVersion(toml.getString("modpack.version", b.modpackVersion));
            b.modpackRevision(toml.getLong("modpack.revision", b.modpackRevision));
            b.minimumRevision(toml.getLong(
                    "modpack.minimum-revision",
                    b.minimumRevision
            ));
            b.recommendedMemoryMinMb(toml.getLong(
                    "modpack.recommended-memory-min-mb",
                    (long) b.recommendedMemoryMinMb
            ).intValue());
            b.recommendedMemoryMaxMb(toml.getLong(
                    "modpack.recommended-memory-max-mb",
                    (long) b.recommendedMemoryMaxMb
            ).intValue());
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

            String unresolved = toml.getString("modpack.unresolved-policy", "server");
            b.unresolvedPolicy(UnresolvedPolicy.parse(unresolved));

            List<String> privatePaths = toml.getList("security.private-paths");
            if (privatePaths != null) {
                b.privatePaths(privatePaths);
            }

            Set<UUID> adminUuids = parseUuidList(toml.getList("security.admin-uuids"));
            b.adminUuids(adminUuids);

            Toml caps = toml.getTable("security.capabilities");
            if (caps != null) {
                b.capabilities(new SecurityCapabilities(
                        CapabilityLevel.parse(caps.getString("public_modpack"), CapabilityLevel.OPEN),
                        CapabilityLevel.parse(caps.getString("private_resources"), CapabilityLevel.DENY),
                        CapabilityLevel.parse(caps.getString("manage_content"), CapabilityLevel.PERM_OR_PASSWORD),
                        adminUuids,
                        toml.getString("security.permissions.install", "vesta.mods.install"),
                        toml.getString("security.permissions.private_read", "vesta.files.private"),
                        toml.getBoolean("security.op-grants-manage", true),
                        privatePaths != null ? privatePaths : List.of()
                ));
            }

            log.info("Loaded config from {}", file);
        } catch (Exception e) {
            log.error("Failed to parse config: {}, using defaults", e.getMessage());
        }

        return b.build();
    }

    private static Set<UUID> parseUuidList(List<String> raw) {
        Set<UUID> set = new HashSet<>();
        if (raw == null) {
            return set;
        }
        for (String s : raw) {
            if (s != null && !s.trim().isEmpty()) {
                set.add(UUID.fromString(s.trim()));
            }
        }
        return set;
    }

    private ConfigLoader() {}
}
