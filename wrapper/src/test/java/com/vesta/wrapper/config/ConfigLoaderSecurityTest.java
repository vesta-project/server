package com.vesta.wrapper.config;

import com.vesta.sync.auth.SecurityMode;
import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.io.TempDir;

import java.nio.file.Files;
import java.nio.file.Path;

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
    }
}
