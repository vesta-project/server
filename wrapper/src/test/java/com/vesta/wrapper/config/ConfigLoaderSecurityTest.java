package com.vesta.wrapper.config;

import com.vesta.sync.auth.SecurityMode;
import com.vesta.sync.manifest.PackSource;
import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.io.TempDir;

import java.nio.file.Files;
import java.nio.file.Path;
import java.util.List;
import java.util.UUID;

import static org.junit.jupiter.api.Assertions.*;

class ConfigLoaderSecurityTest {

    @TempDir
    Path temp;

    @Test
    void loadsPasswordMode() throws Exception {
        Path file = temp.resolve("wrapper-config.toml");
        Files.writeString(file, """
                [security]
                mode = "password"
                password = "s3cret"
                token-ttl-seconds = 120
                control-port = 9191
                """);
        WrapperConfig config = ConfigLoader.load(file);
        assertEquals(SecurityMode.PASSWORD, config.securityMode);
        assertEquals("s3cret", config.securityPassword);
        assertEquals(120, config.tokenTtlSeconds);
        assertEquals(9191, config.controlPort);
    }

    @Test
    void defaultsToOpen() {
        WrapperConfig config = ConfigLoader.load(temp.resolve("missing.toml"));
        assertEquals(SecurityMode.OPEN, config.securityMode);
        assertNotNull(config.serverId);
        assertEquals(1L, config.modpackRevision);
        assertEquals(1L, config.minimumRevision);
    }

    @Test
    void loadsIdentityRevisionAndMemoryGuidance() throws Exception {
        UUID serverId = UUID.randomUUID();
        Path file = temp.resolve("wrapper-config.toml");
        Files.writeString(file, """
                [server.identity]
                id = "%s"
                name = "Friends SMP"
                banner-url = "https://example.test/banner.webp"

                [modpack]
                revision = 9
                minimum-revision = 7
                recommended-memory-min-mb = 4096
                recommended-memory-max-mb = 8192
                """.formatted(serverId));

        WrapperConfig config = ConfigLoader.load(file);
        assertEquals(serverId, config.serverId);
        assertEquals("Friends SMP", config.serverDisplayName);
        assertEquals(9L, config.modpackRevision);
        assertEquals(7L, config.minimumRevision);
        assertEquals(4096, config.recommendedMemoryMinMb);
        assertEquals(8192, config.recommendedMemoryMaxMb);
    }

    @Test
    void loadsPlatformPackReference() throws Exception {
        Path file = temp.resolve("wrapper-config.toml");
        Files.writeString(file, """
                [modpack]
                source = "modrinth"

                [modpack.platform]
                project-id = "AANobbMI"
                version-id = "example-version"
                download-url = "https://cdn.example.test/pack.mrpack"
                """);

        WrapperConfig config = ConfigLoader.load(file);
        assertEquals(PackSource.MODRINTH, config.modpackSource);
        assertEquals("AANobbMI", config.modpackPlatformProjectId);
        assertEquals("example-version", config.modpackPlatformVersionId);
        assertEquals(
                "https://cdn.example.test/pack.mrpack",
                config.modpackPlatformDownloadUrl
        );
    }

    @Test
    void loadsLegacyPlatformSourceForCompatibility() throws Exception {
        Path file = temp.resolve("wrapper-config.toml");
        Files.writeString(file, """
                [modpack.platform]
                source = "curseforge"
                project-id = "123"
                version-id = "456"
                """);

        WrapperConfig config = ConfigLoader.load(file);
        assertEquals(PackSource.CURSEFORGE, config.modpackSource);
        assertEquals("123", config.modpackPlatformProjectId);
        assertEquals("456", config.modpackPlatformVersionId);
    }

    @Test
    void loadsLocalArchivePath() throws Exception {
        Path file = temp.resolve("wrapper-config.toml");
        Files.writeString(file, """
                [modpack]
                source = "archive"

                [modpack.archive]
                path = "packs/friends.mrpack"
                """);

        WrapperConfig config = ConfigLoader.load(file);
        assertEquals(PackSource.ARCHIVE, config.modpackSource);
        assertEquals("packs/friends.mrpack", config.modpackArchivePath);
    }

    @Test
    void defaultsToSelfHostedSource() {
        WrapperConfig config = ConfigLoader.load(temp.resolve("missing.toml"));
        assertEquals(PackSource.SELF_HOSTED, config.modpackSource);
    }

    @Test
    void loadsMinecraftLaunchConfiguration() throws Exception {
        Path file = temp.resolve("wrapper-config.toml");
        Files.writeString(file, """
                [minecraft]
                auto-start = true
                launch-jar = "fabric-server.jar"
                working-directory = "%s"
                java-command = "custom-java"
                jvm-args = ["-Xms1G", "-Xmx3G"]
                server-args = ["nogui", "--demo"]
                """.formatted(temp.toString().replace("\\", "/")));

        WrapperConfig config = ConfigLoader.load(file);
        assertTrue(config.minecraftAutoStart);
        assertEquals("custom-java", config.minecraftJavaCommand);
        assertEquals(List.of("-Xms1G", "-Xmx3G"), config.minecraftJvmArgs);
        assertEquals(List.of("nogui", "--demo"), config.minecraftServerArgs);
        assertEquals(temp.resolve("fabric-server.jar"), config.minecraftLaunchJar);
    }
}
