package com.vesta.wrapper.config;

import java.nio.file.Path;

public final class WrapperConfig {

    public final int tcpPort;
    public final int httpPort;
    public final Path fileStoreDir;
    public final String modpackName;
    public final String modpackVersion;
    public final String minecraftVersion;
    public final String modloaderType;
    public final String modloaderVersion;
    public final String serverHost;
    public final int serverPort;
    public final String serverDescription;
    public final boolean serverRequired;

    private WrapperConfig(Builder b) {
        this.tcpPort = b.tcpPort;
        this.httpPort = b.httpPort;
        this.fileStoreDir = b.fileStoreDir;
        this.modpackName = b.modpackName;
        this.modpackVersion = b.modpackVersion;
        this.minecraftVersion = b.minecraftVersion;
        this.modloaderType = b.modloaderType;
        this.modloaderVersion = b.modloaderVersion;
        this.serverHost = b.serverHost;
        this.serverPort = b.serverPort;
        this.serverDescription = b.serverDescription;
        this.serverRequired = b.serverRequired;
    }

    public static class Builder {
        int tcpPort = 9091;
        int httpPort = 9090;
        Path fileStoreDir = Path.of("vesta-files");
        String modpackName = "Server Modpack";
        String modpackVersion = "1.0.0";
        String minecraftVersion = "1.21.4";
        String modloaderType = "fabric";
        String modloaderVersion = "0.18.4";
        String serverHost = "localhost";
        int serverPort = 25565;
        String serverDescription = "";
        boolean serverRequired = false;

        public Builder tcpPort(int v) { tcpPort = v; return this; }
        public Builder httpPort(int v) { httpPort = v; return this; }
        public Builder fileStoreDir(Path v) { fileStoreDir = v; return this; }
        public Builder modpackName(String v) { modpackName = v; return this; }
        public Builder modpackVersion(String v) { modpackVersion = v; return this; }
        public Builder minecraftVersion(String v) { minecraftVersion = v; return this; }
        public Builder modloaderType(String v) { modloaderType = v; return this; }
        public Builder modloaderVersion(String v) { modloaderVersion = v; return this; }
        public Builder serverHost(String v) { serverHost = v; return this; }
        public Builder serverPort(int v) { serverPort = v; return this; }
        public Builder serverDescription(String v) { serverDescription = v; return this; }
        public Builder serverRequired(boolean v) { serverRequired = v; return this; }

        public WrapperConfig build() {
            return new WrapperConfig(this);
        }
    }
}
