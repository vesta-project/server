package com.vesta.wrapper.config;

import com.vesta.sync.auth.CapabilityLevel;
import com.vesta.sync.auth.SecurityCapabilities;
import com.vesta.sync.auth.SecurityMode;
import com.vesta.sync.manifest.UnresolvedPolicy;

import java.nio.file.Path;
import java.nio.charset.StandardCharsets;
import java.util.ArrayList;
import java.util.Collections;
import java.util.HashSet;
import java.util.List;
import java.util.Set;
import java.util.UUID;

public final class WrapperConfig {

    public final int tcpPort;
    public final int httpPort;
    public final Path scanRoot;
    public final List<String> scanPaths;
    public final String publicScheme;
    public final String publicHost;
    public final int publicPort;
    public final UUID serverId;
    public final String serverDisplayName;
    public final String serverBannerUrl;
    public final String modpackName;
    public final String modpackDescription;
    public final String modpackVersion;
    public final long modpackRevision;
    public final long minimumRevision;
    public final int recommendedMemoryMinMb;
    public final int recommendedMemoryMaxMb;
    public final String minecraftVersion;
    public final String modloaderType;
    public final String modloaderVersion;
    public final String serverHost;
    public final int serverPort;
    public final String serverDescription;
    public final boolean serverRequired;
    public final SecurityMode securityMode;
    public final String securityPassword;
    public final long tokenTtlSeconds;
    public final int controlPort;
    public final UnresolvedPolicy unresolvedPolicy;
    public final SecurityCapabilities capabilities;
    public final Set<UUID> adminUuids;
    public final List<String> privatePaths;

    private WrapperConfig(Builder b) {
        this.tcpPort = b.tcpPort;
        this.httpPort = b.httpPort;
        this.scanRoot = b.scanRoot;
        this.scanPaths = b.scanPaths;
        this.publicScheme = b.publicScheme;
        this.publicHost = b.publicHost;
        this.publicPort = b.publicPort;
        this.serverId = b.serverId != null
                ? b.serverId
                : UUID.nameUUIDFromBytes(
                        ("vesta:" + b.scanRoot.toAbsolutePath().normalize())
                                .getBytes(StandardCharsets.UTF_8)
                );
        this.serverDisplayName = b.serverDisplayName;
        this.serverBannerUrl = b.serverBannerUrl;
        this.modpackName = b.modpackName;
        this.modpackDescription = b.modpackDescription;
        this.modpackVersion = b.modpackVersion;
        this.modpackRevision = Math.max(1L, b.modpackRevision);
        this.minimumRevision = Math.max(1L, Math.min(this.modpackRevision, b.minimumRevision));
        this.recommendedMemoryMinMb = Math.max(0, b.recommendedMemoryMinMb);
        this.recommendedMemoryMaxMb = Math.max(
                this.recommendedMemoryMinMb,
                b.recommendedMemoryMaxMb
        );
        this.minecraftVersion = b.minecraftVersion;
        this.modloaderType = b.modloaderType;
        this.modloaderVersion = b.modloaderVersion;
        this.serverHost = b.serverHost;
        this.serverPort = b.serverPort;
        this.serverDescription = b.serverDescription;
        this.serverRequired = b.serverRequired;
        this.securityMode = b.securityMode;
        this.securityPassword = b.securityPassword;
        this.tokenTtlSeconds = b.tokenTtlSeconds;
        this.controlPort = b.controlPort;
        this.unresolvedPolicy = b.unresolvedPolicy;
        this.adminUuids = Collections.unmodifiableSet(new HashSet<>(b.adminUuids));
        this.privatePaths = Collections.unmodifiableList(new ArrayList<>(b.privatePaths));
        this.capabilities = b.capabilities != null ? b.capabilities : defaultCapabilities(b.securityMode, b.adminUuids, b.privatePaths);
    }

    private static SecurityCapabilities defaultCapabilities(
            SecurityMode mode,
            Set<UUID> adminUuids,
            List<String> privatePaths
    ) {
        CapabilityLevel publicLevel = mode == SecurityMode.OPEN ? CapabilityLevel.OPEN : CapabilityLevel.AUTH;
        CapabilityLevel privateLevel = mode == SecurityMode.OPEN
                ? CapabilityLevel.DENY
                : (mode == SecurityMode.WHITELIST ? CapabilityLevel.WHITELIST : CapabilityLevel.LINKED);
        return new SecurityCapabilities(
                publicLevel,
                privateLevel,
                CapabilityLevel.PERM_OR_PASSWORD,
                adminUuids,
                "vesta.mods.install",
                "vesta.files.private",
                true,
                privatePaths
        );
    }

    public int effectivePublicPort() {
        return publicPort > 0 ? publicPort : (tcpPort == httpPort ? tcpPort : httpPort);
    }

    public static class Builder {
        int tcpPort = 9090;
        int httpPort = 9090;
        Path scanRoot = Path.of(".");
        List<String> scanPaths = List.of("mods", "config");
        String publicScheme = "http";
        String publicHost = "localhost";
        int publicPort = 0;
        UUID serverId;
        String serverDisplayName = "Vesta Server";
        String serverBannerUrl = "";
        String modpackName = "Server Modpack";
        String modpackDescription = "";
        String modpackVersion = "1.0.0";
        long modpackRevision = 1L;
        long minimumRevision = 1L;
        int recommendedMemoryMinMb = 0;
        int recommendedMemoryMaxMb = 0;
        String minecraftVersion = "1.21.4";
        String modloaderType = "fabric";
        String modloaderVersion = "0.18.4";
        String serverHost = "localhost";
        int serverPort = 25565;
        String serverDescription = "";
        boolean serverRequired = false;
        SecurityMode securityMode = SecurityMode.OPEN;
        String securityPassword = "";
        long tokenTtlSeconds = 86_400L;
        int controlPort = 9091;
        UnresolvedPolicy unresolvedPolicy = UnresolvedPolicy.SERVER;
        SecurityCapabilities capabilities;
        Set<UUID> adminUuids = new HashSet<>();
        List<String> privatePaths = new ArrayList<>();

        public Builder tcpPort(int v) { tcpPort = v; return this; }
        public Builder httpPort(int v) { httpPort = v; return this; }
        public Builder scanRoot(Path v) { scanRoot = v; return this; }
        public Builder scanPaths(List<String> v) { scanPaths = v; return this; }
        public Builder publicScheme(String v) { publicScheme = v; return this; }
        public Builder publicHost(String v) { publicHost = v; return this; }
        public Builder publicPort(int v) { publicPort = v; return this; }
        public Builder serverId(UUID v) { serverId = v; return this; }
        public Builder serverDisplayName(String v) { serverDisplayName = v; return this; }
        public Builder serverBannerUrl(String v) { serverBannerUrl = v; return this; }
        public Builder modpackName(String v) { modpackName = v; return this; }
        public Builder modpackDescription(String v) { modpackDescription = v; return this; }
        public Builder modpackVersion(String v) { modpackVersion = v; return this; }
        public Builder modpackRevision(long v) { modpackRevision = v; return this; }
        public Builder minimumRevision(long v) { minimumRevision = v; return this; }
        public Builder recommendedMemoryMinMb(int v) { recommendedMemoryMinMb = v; return this; }
        public Builder recommendedMemoryMaxMb(int v) { recommendedMemoryMaxMb = v; return this; }
        public Builder minecraftVersion(String v) { minecraftVersion = v; return this; }
        public Builder modloaderType(String v) { modloaderType = v; return this; }
        public Builder modloaderVersion(String v) { modloaderVersion = v; return this; }
        public Builder serverHost(String v) { serverHost = v; return this; }
        public Builder serverPort(int v) { serverPort = v; return this; }
        public Builder serverDescription(String v) { serverDescription = v; return this; }
        public Builder serverRequired(boolean v) { serverRequired = v; return this; }
        public Builder securityMode(SecurityMode v) { securityMode = v; return this; }
        public Builder securityPassword(String v) { securityPassword = v; return this; }
        public Builder tokenTtlSeconds(long v) { tokenTtlSeconds = v; return this; }
        public Builder controlPort(int v) { controlPort = v; return this; }
        public Builder unresolvedPolicy(UnresolvedPolicy v) { unresolvedPolicy = v; return this; }
        public Builder capabilities(SecurityCapabilities v) { capabilities = v; return this; }
        public Builder adminUuids(Set<UUID> v) { adminUuids = new HashSet<>(v); return this; }
        public Builder privatePaths(List<String> v) { privatePaths = new ArrayList<>(v); return this; }

        public WrapperConfig build() {
            return new WrapperConfig(this);
        }
    }
}
