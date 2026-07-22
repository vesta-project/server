package com.vesta.wrapper.runtime;

import com.vesta.wrapper.config.WrapperConfig;
import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.io.TempDir;

import java.nio.file.Path;
import java.time.Duration;
import java.util.List;
import java.util.Map;

import static org.junit.jupiter.api.Assertions.*;

class PlayersQueryTest {

    @TempDir
    Path root;

    @Test
    void notManageableReturnsEmptyPlayers() {
        WrapperConfig config = new WrapperConfig.Builder()
                .scanRoot(root)
                .tlsEnabled(false)
                .build();
        MinecraftServerProcess process = new MinecraftServerProcess(config);
        Map<String, Object> result = new PlayersQuery(Duration.ofMillis(200)).query(process);
        assertEquals(false, result.get("manageable"));
        assertEquals(List.of(), result.get("players"));
    }

    @Test
    void manageableButStoppedReturnsEmptyWithoutError() throws Exception {
        Path jar = root.resolve("server.jar");
        java.nio.file.Files.writeString(jar, "fake");
        WrapperConfig config = new WrapperConfig.Builder()
                .scanRoot(root)
                .tlsEnabled(false)
                .minecraftLaunchJar(jar)
                .minecraftWorkingDirectory(root)
                .build();
        MinecraftServerProcess process = new MinecraftServerProcess(config);
        Map<String, Object> result = new PlayersQuery(Duration.ofMillis(200)).query(process);
        assertEquals(true, result.get("manageable"));
        assertEquals(false, result.get("running"));
        assertEquals(0, result.get("online"));
        assertEquals(List.of(), result.get("players"));
        assertFalse(result.containsKey("error"));
    }
}
