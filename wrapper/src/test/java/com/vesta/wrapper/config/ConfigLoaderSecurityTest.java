package com.vesta.wrapper.config;

import com.vesta.sync.auth.SecurityMode;
import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.io.TempDir;

import java.nio.file.Files;
import java.nio.file.Path;
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
}
