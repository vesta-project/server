package com.vesta.wrapper.config;

import com.moandjiezana.toml.Toml;
import com.vesta.sync.auth.AccessCapability;
import com.vesta.sync.auth.AccessControlConfig;
import com.vesta.sync.auth.CapabilityLevel;
import com.vesta.sync.auth.SecurityCapabilities;
import com.vesta.sync.auth.SecurityMode;
import com.vesta.sync.manifest.PackSource;
import com.vesta.sync.manifest.UnresolvedPolicy;
import org.slf4j.Logger;
import org.slf4j.LoggerFactory;

import java.nio.file.Files;
import java.nio.file.Path;
import java.util.ArrayList;
import java.util.Collections;
import java.util.HashMap;
import java.util.HashSet;
import java.util.LinkedHashMap;
import java.util.LinkedHashSet;
import java.util.List;
import java.util.Map;
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

            // Prefer top-level modpack.source. Fall back to legacy [modpack.platform].source.
            String topLevelSource = toml.getString("modpack.source");
            String legacyPlatformSource = toml.getString("modpack.platform.source", "");
            if (topLevelSource != null && !topLevelSource.isBlank()) {
                b.modpackSource(PackSource.parse(topLevelSource));
            } else if (legacyPlatformSource != null && !legacyPlatformSource.isBlank()) {
                b.modpackSource(PackSource.parse(legacyPlatformSource));
            } else {
                b.modpackSource(PackSource.SELF_HOSTED);
            }

            b.modpackPlatformProjectId(toml.getString(
                    "modpack.platform.project-id",
                    b.modpackPlatformProjectId
            ).trim());
            b.modpackPlatformVersionId(toml.getString(
                    "modpack.platform.version-id",
                    b.modpackPlatformVersionId
            ).trim());
            b.modpackPlatformDownloadUrl(toml.getString(
                    "modpack.platform.download-url",
                    b.modpackPlatformDownloadUrl
            ).trim());
            b.modpackArchivePath(toml.getString(
                    "modpack.archive.path",
                    b.modpackArchivePath
            ).trim());
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

            b.minecraftAutoStart(toml.getBoolean(
                    "minecraft.auto-start",
                    b.minecraftAutoStart
            ));
            b.minecraftJavaCommand(toml.getString(
                    "minecraft.java-command",
                    b.minecraftJavaCommand
            ));
            List<String> jvmArgs = toml.getList("minecraft.jvm-args");
            if (jvmArgs != null) b.minecraftJvmArgs(jvmArgs);
            List<String> serverArgs = toml.getList("minecraft.server-args");
            if (serverArgs != null) b.minecraftServerArgs(serverArgs);
            String workingDirectory = toml.getString("minecraft.working-directory");
            if (workingDirectory != null && !workingDirectory.isBlank()) {
                b.minecraftWorkingDirectory(Path.of(workingDirectory));
            }
            String launchJar = toml.getString("minecraft.launch-jar");
            if (launchJar != null && !launchJar.isBlank()) {
                b.minecraftLaunchJar(Path.of(launchJar));
            }

            b.connectorAutoUpdate(toml.getBoolean(
                    "connector.auto-update",
                    b.connectorAutoUpdate
            ));

            b.devtoolsEnabled(toml.getBoolean(
                    "devtools.enabled",
                    b.devtoolsEnabled
            ));
            b.devtoolsLoopbackOnly(toml.getBoolean(
                    "devtools.loopback_only",
                    b.devtoolsLoopbackOnly
            ));

            b.rateLimitsEnabled(toml.getBoolean(
                    "rate_limits.enabled",
                    b.rateLimitsEnabled
            ));
            b.httpRequestsPerMinute(toml.getLong(
                    "rate_limits.http_requests_per_minute",
                    (long) b.httpRequestsPerMinute
            ).intValue());
            b.websocketMessagesPerSecond(toml.getLong(
                    "rate_limits.websocket_messages_per_second",
                    (long) b.websocketMessagesPerSecond
            ).intValue());
            b.authAttemptsPerMinute(toml.getLong(
                    "rate_limits.auth_attempts_per_minute",
                    (long) b.authAttemptsPerMinute
            ).intValue());

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

            Toml securityTable = toml.getTable("security");
            Toml levelsTable = toml.getTable("security.levels");
            Toml peopleTable = toml.getTable("security.people");
            Toml permNodesTable = toml.getTable("security.perm-nodes");
            Toml caps = toml.getTable("security.capabilities");
            Map<String, Object> securityMap = securityTable != null
                    ? securityTable.toMap()
                    : Collections.<String, Object>emptyMap();
            boolean hasAccessLevels = levelsTable != null
                    || peopleTable != null
                    || permNodesTable != null
                    || securityMap.containsKey("anonymous-level")
                    || securityMap.containsKey("authenticated-level")
                    || securityMap.containsKey("password-level")
                    || securityMap.containsKey("linked-level")
                    || securityMap.containsKey("whitelist-level");
            if (hasAccessLevels) {
                b.capabilities(new SecurityCapabilities(loadAccessControl(
                        toml,
                        levelsTable,
                        peopleTable,
                        permNodesTable,
                        adminUuids,
                        privatePaths
                )));
            } else if (caps != null) {
                b.capabilities(SecurityCapabilities.fromLegacy(
                        CapabilityLevel.parse(caps.getString("public_modpack"), CapabilityLevel.OPEN),
                        CapabilityLevel.parse(caps.getString("private_resources"), CapabilityLevel.DENY),
                        CapabilityLevel.parse(caps.getString("manage_content"), CapabilityLevel.PERM_OR_PASSWORD),
                        adminUuids,
                        toml.getString("security.permissions.install", "vesta.mods.install"),
                        toml.getString("security.permissions.private_read", "vesta.files.private"),
                        toml.getBoolean("security.op-grants-manage", true),
                        privatePaths != null ? privatePaths : Collections.<String>emptyList()
                ));
            }

            log.info("Loaded config from {}", file);
        } catch (Exception e) {
            log.error("Failed to parse config: {}, using defaults", e.getMessage());
        }

        return b.build();
    }

    private static AccessControlConfig loadAccessControl(
            Toml toml,
            Toml levelsTable,
            Toml peopleTable,
            Toml permNodesTable,
            Set<UUID> adminUuids,
            List<String> privatePaths
    ) {
        AccessControlConfig defaults = AccessControlConfig.defaults();
        Map<Integer, Set<String>> levels = new HashMap<Integer, Set<String>>(defaults.levels);
        if (levelsTable != null) {
            for (int i = 0; i <= 4; i++) {
                List<String> caps = levelCapabilityList(levelsTable, i);
                if (caps != null) {
                    Set<String> normalized = new LinkedHashSet<String>();
                    for (String cap : caps) {
                        normalized.add(AccessCapability.requireValid(cap));
                    }
                    levels.put(Integer.valueOf(i), normalized);
                }
            }
        }

        Map<UUID, Integer> people = new LinkedHashMap<UUID, Integer>();
        if (adminUuids != null) {
            for (UUID uuid : adminUuids) {
                people.put(uuid, Integer.valueOf(4));
            }
        }
        if (peopleTable != null) {
            // Prefer [[security.people]] entries: { uuid = "...", level = 4 }
            List<Toml> personRows = peopleTable.getTables("entry");
            if (personRows != null) {
                for (Toml row : personRows) {
                    String raw = row.getString("uuid");
                    if (raw != null && !raw.trim().isEmpty()) {
                        people.put(UUID.fromString(raw.trim()), Integer.valueOf(
                                row.getLong("level", 4L).intValue()
                        ));
                    }
                }
            }
            Map<String, Object> values = peopleTable.toMap();
            for (Map.Entry<String, Object> entry : values.entrySet()) {
                String key = unwrapTomlKey(entry.getKey());
                if (key.isEmpty() || "entry".equals(key)) {
                    continue;
                }
                try {
                    people.put(UUID.fromString(key), Integer.valueOf(asInt(entry.getValue(), 4)));
                } catch (IllegalArgumentException ignored) {
                    // Skip non-UUID keys (comments / nested tables).
                }
            }
        }

        Map<String, Integer> permNodes = new LinkedHashMap<String, Integer>();
        if (permNodesTable != null) {
            Map<String, Object> values = permNodesTable.toMap();
            for (Map.Entry<String, Object> entry : values.entrySet()) {
                String node = unwrapTomlKey(entry.getKey());
                if (!node.isEmpty()) {
                    permNodes.put(node, Integer.valueOf(asInt(entry.getValue(), 4)));
                }
            }
        } else {
            // Preserve legacy permission keys when only baselines/levels were customized.
            String install = toml.getString("security.permissions.install", "");
            String privateRead = toml.getString("security.permissions.private_read", "");
            if (install != null && !install.trim().isEmpty()) {
                permNodes.put(install.trim(), Integer.valueOf(4));
            }
            if (privateRead != null && !privateRead.trim().isEmpty()) {
                permNodes.put(privateRead.trim(), Integer.valueOf(3));
            }
        }

        return new AccessControlConfig(
                levels,
                toml.getLong("security.anonymous-level", (long) defaults.anonymousBaseline).intValue(),
                toml.getLong("security.authenticated-level", (long) defaults.authenticatedBaseline).intValue(),
                toml.getLong("security.password-level", (long) defaults.passwordBaseline).intValue(),
                toml.getLong("security.linked-level", (long) defaults.linkedBaseline).intValue(),
                toml.getLong("security.whitelist-level", (long) defaults.whitelistBaseline).intValue(),
                people,
                permNodes,
                privatePaths != null ? privatePaths : Collections.<String>emptyList()
        );
    }

    @SuppressWarnings("unchecked")
    private static List<String> levelCapabilityList(Toml levelsTable, int level) {
        Map<String, Object> map = levelsTable.toMap();
        Object value = map.get(String.valueOf(level));
        if (value == null) {
            value = map.get("\"" + level + "\"");
        }
        if (value == null) {
            value = map.get(Long.valueOf(level));
        }
        if (value == null) {
            value = map.get(Integer.valueOf(level));
        }
        if (value == null) {
            for (Map.Entry<String, Object> entry : map.entrySet()) {
                if (unwrapTomlKey(entry.getKey()).equals(String.valueOf(level))) {
                    value = entry.getValue();
                    break;
                }
            }
        }
        if (value instanceof List) {
            List<String> out = new ArrayList<String>();
            for (Object item : (List<Object>) value) {
                if (item != null) {
                    out.add(String.valueOf(item));
                }
            }
            return out;
        }
        return null;
    }

    private static String unwrapTomlKey(String key) {
        if (key == null) {
            return "";
        }
        String value = key.trim();
        if (value.length() >= 2 && value.charAt(0) == '"' && value.charAt(value.length() - 1) == '"') {
            return value.substring(1, value.length() - 1);
        }
        return value;
    }

    private static int asInt(Object value, int fallback) {
        if (value instanceof Number) {
            return ((Number) value).intValue();
        }
        if (value instanceof String) {
            try {
                return Integer.parseInt(((String) value).trim());
            } catch (NumberFormatException ignored) {
                return fallback;
            }
        }
        return fallback;
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
