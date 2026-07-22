package com.vesta.wrapper.runtime;

import com.vesta.wrapper.config.WrapperConfig;
import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.io.TempDir;

import java.nio.file.Files;
import java.nio.file.Path;
import java.util.List;
import java.util.Map;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertThrows;
import static org.junit.jupiter.api.Assertions.assertTrue;

class MinecraftServerProcessTest {
    @TempDir
    Path root;

    @Test
    void buildsConfiguredJavaJarCommand() {
        Path serverRoot = Path.of("server-root").toAbsolutePath();
        WrapperConfig config = new WrapperConfig.Builder()
                .scanRoot(serverRoot)
                .minecraftWorkingDirectory(serverRoot)
                .minecraftLaunchJar(Path.of("fabric-server.jar"))
                .minecraftJavaCommand("custom-java")
                .minecraftJvmArgs(List.of("-Xms1G", "-Xmx3G"))
                .minecraftServerArgs(List.of("nogui"))
                .build();

        assertEquals(
                List.of(
                        "custom-java",
                        "-Xms1G",
                        "-Xmx3G",
                        "-jar",
                        serverRoot.resolve("fabric-server.jar").toString(),
                        "nogui"
                ),
                new MinecraftServerProcess(config).command()
        );
    }

    @Test
    void snapshotReportsStoppedWhenNotRunning() throws Exception {
        Path jar = root.resolve("server.jar");
        Files.writeString(jar, "dummy");
        WrapperConfig config = new WrapperConfig.Builder()
                .scanRoot(root)
                .minecraftWorkingDirectory(root)
                .minecraftLaunchJar(jar)
                .build();
        MinecraftServerProcess process = new MinecraftServerProcess(config);
        Map<String, Object> snap = process.snapshot();
        assertEquals(true, snap.get("configured"));
        assertEquals(false, snap.get("running"));
        assertEquals("stopped", snap.get("state"));
        assertEquals(MinecraftServerProcess.State.STOPPED, process.state());
        assertFalse(process.isRunning());
    }

    @Test
    void killAndStopAreSafeWhenNotRunning() throws Exception {
        Path jar = root.resolve("server.jar");
        Files.writeString(jar, "dummy");
        WrapperConfig config = new WrapperConfig.Builder()
                .scanRoot(root)
                .minecraftWorkingDirectory(root)
                .minecraftLaunchJar(jar)
                .build();
        MinecraftServerProcess process = new MinecraftServerProcess(config);
        process.stop();
        process.kill();
        assertTrue(process.snapshot().get("state").equals("stopped"));
    }

    @Test
    void sendCommandFailsWhenNotRunning() throws Exception {
        Path jar = root.resolve("server.jar");
        Files.writeString(jar, "dummy");
        WrapperConfig config = new WrapperConfig.Builder()
                .scanRoot(root)
                .minecraftWorkingDirectory(root)
                .minecraftLaunchJar(jar)
                .build();
        MinecraftServerProcess process = new MinecraftServerProcess(config);
        assertThrows(java.io.IOException.class, () -> process.sendCommand("list"));
        assertThrows(IllegalArgumentException.class, () -> process.sendCommand("  "));
    }
}
