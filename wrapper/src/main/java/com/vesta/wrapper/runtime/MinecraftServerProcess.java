package com.vesta.wrapper.runtime;

import com.vesta.wrapper.config.WrapperConfig;
import org.slf4j.Logger;
import org.slf4j.LoggerFactory;

import java.io.IOException;
import java.nio.file.Files;
import java.time.Duration;
import java.util.ArrayList;
import java.util.List;
import java.util.concurrent.TimeUnit;

public final class MinecraftServerProcess {
    private static final Logger log = LoggerFactory.getLogger(MinecraftServerProcess.class);
    private static final Duration STOP_TIMEOUT = Duration.ofSeconds(10);

    private final WrapperConfig config;
    private volatile Process process;

    public MinecraftServerProcess(WrapperConfig config) {
        this.config = config;
    }

    public List<String> command() {
        if (config.minecraftLaunchJar == null) {
            return List.of();
        }
        List<String> command = new ArrayList<>();
        command.add(config.minecraftJavaCommand);
        command.addAll(config.minecraftJvmArgs);
        command.add("-jar");
        command.add(config.minecraftLaunchJar.toString());
        command.addAll(config.minecraftServerArgs);
        return List.copyOf(command);
    }

    public synchronized void startIfConfigured() throws IOException {
        if (!config.minecraftAutoStart) {
            return;
        }
        if (config.minecraftLaunchJar == null) {
            throw new IOException("minecraft.auto-start requires minecraft.launch-jar");
        }
        if (!Files.isRegularFile(config.minecraftLaunchJar)) {
            throw new IOException("Minecraft launch jar does not exist: " + config.minecraftLaunchJar);
        }
        if (process != null && process.isAlive()) {
            return;
        }

        ProcessBuilder builder = new ProcessBuilder(command());
        builder.directory(config.minecraftWorkingDirectory.toFile());
        builder.redirectErrorStream(true);
        builder.inheritIO();
        process = builder.start();
        log.info("Started Minecraft server process (pid {})", process.pid());

        Process launched = process;
        Thread watcher = new Thread(() -> {
            try {
                int exitCode = launched.waitFor();
                log.info("Minecraft server process exited with code {}", exitCode);
            } catch (InterruptedException interrupted) {
                Thread.currentThread().interrupt();
            }
        }, "vesta-minecraft-process");
        watcher.setDaemon(true);
        watcher.start();
    }

    public synchronized void stop() {
        Process current = process;
        if (current == null || !current.isAlive()) {
            return;
        }
        current.destroy();
        try {
            if (!current.waitFor(STOP_TIMEOUT.toSeconds(), TimeUnit.SECONDS)) {
                log.warn("Minecraft server did not stop in time; terminating it");
                current.destroyForcibly();
            }
        } catch (InterruptedException interrupted) {
            Thread.currentThread().interrupt();
            current.destroyForcibly();
        }
    }
}
