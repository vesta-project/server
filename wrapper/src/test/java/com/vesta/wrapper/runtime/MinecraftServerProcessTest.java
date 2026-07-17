package com.vesta.wrapper.runtime;

import com.vesta.wrapper.config.WrapperConfig;
import org.junit.jupiter.api.Test;

import java.nio.file.Path;
import java.util.List;

import static org.junit.jupiter.api.Assertions.assertEquals;

class MinecraftServerProcessTest {
    @Test
    void buildsConfiguredJavaJarCommand() {
        Path root = Path.of("server-root").toAbsolutePath();
        WrapperConfig config = new WrapperConfig.Builder()
                .scanRoot(root)
                .minecraftWorkingDirectory(root)
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
                        root.resolve("fabric-server.jar").toString(),
                        "nogui"
                ),
                new MinecraftServerProcess(config).command()
        );
    }
}
