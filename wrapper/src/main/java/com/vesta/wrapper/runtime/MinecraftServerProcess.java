package com.vesta.wrapper.runtime;

import com.vesta.wrapper.config.WrapperConfig;
import org.slf4j.Logger;
import org.slf4j.LoggerFactory;

import java.io.BufferedWriter;
import java.io.IOException;
import java.io.OutputStreamWriter;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.time.Duration;
import java.time.Instant;
import java.util.ArrayList;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Locale;
import java.util.Map;
import java.util.concurrent.TimeUnit;

public final class MinecraftServerProcess {
    private static final Logger log = LoggerFactory.getLogger(MinecraftServerProcess.class);
    private static final Duration STOP_TIMEOUT = Duration.ofSeconds(10);

    public enum State {
        STOPPED,
        STARTING,
        RUNNING,
        STOPPING
    }

    private final WrapperConfig config;
    private volatile Process process;
    private volatile BufferedWriter stdin;
    private volatile State state = State.STOPPED;
    private volatile Long pid;
    private volatile Instant startedAt;
    private volatile Integer lastExitCode;

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
        start();
    }

    /** True when this wrapper can launch a Minecraft server (a launch jar is configured). */
    public boolean isConfigured() {
        return config.minecraftLaunchJar != null;
    }

    public synchronized boolean isRunning() {
        return process != null && process.isAlive();
    }

    public synchronized State state() {
        if (process != null && process.isAlive()) {
            if (state == State.STARTING) {
                state = State.RUNNING;
            }
            return state == State.STOPPING ? State.STOPPING : State.RUNNING;
        }
        if (state == State.STARTING || state == State.RUNNING || state == State.STOPPING) {
            state = State.STOPPED;
            pid = null;
            startedAt = null;
            stdin = null;
        }
        return state;
    }

    public synchronized Map<String, Object> snapshot() {
        State current = state();
        Map<String, Object> out = new LinkedHashMap<>();
        out.put("configured", isConfigured());
        out.put("manageable", isConfigured());
        out.put("running", current == State.RUNNING || current == State.STARTING || current == State.STOPPING);
        out.put("state", current.name().toLowerCase(Locale.ROOT));
        if (pid != null) {
            out.put("pid", pid);
        }
        if (startedAt != null) {
            out.put("started_at", startedAt.toString());
        }
        if (lastExitCode != null) {
            out.put("last_exit_code", lastExitCode);
        }
        return out;
    }

    /**
     * Starts the configured Minecraft server if it is not already running.
     * Unlike {@link #startIfConfigured()} this ignores the {@code auto-start} flag,
     * so it can be triggered on demand from the control API.
     *
     * @return {@code true} if a new process was launched, {@code false} if one was already alive.
     */
    public synchronized boolean start() throws IOException {
        if (config.minecraftLaunchJar == null) {
            throw new IOException("minecraft.launch-jar is not configured");
        }
        if (!Files.isRegularFile(config.minecraftLaunchJar)) {
            throw new IOException("Minecraft launch jar does not exist: " + config.minecraftLaunchJar);
        }
        if (process != null && process.isAlive()) {
            return false;
        }

        state = State.STARTING;
        ProcessBuilder builder = new ProcessBuilder(command());
        builder.directory(config.minecraftWorkingDirectory.toFile());
        builder.redirectErrorStream(true);
        // Capture stdout so stdin stays writable for the graceful "stop" command.
        // Inherit only the console for visibility by redirecting stdout to inherit while
        // still piping stdin. Java allows redirectInput PIPE + redirectOutput INHERIT.
        builder.redirectInput(ProcessBuilder.Redirect.PIPE);
        builder.redirectOutput(ProcessBuilder.Redirect.INHERIT);
        builder.redirectError(ProcessBuilder.Redirect.INHERIT);
        process = builder.start();
        stdin = new BufferedWriter(new OutputStreamWriter(process.getOutputStream(), StandardCharsets.UTF_8));
        pid = process.pid();
        startedAt = Instant.now();
        lastExitCode = null;
        state = State.RUNNING;
        log.info("Started Minecraft server process (pid {})", pid);

        Process launched = process;
        Thread watcher = new Thread(() -> {
            try {
                int exitCode = launched.waitFor();
                synchronized (MinecraftServerProcess.this) {
                    if (this.process == launched) {
                        lastExitCode = exitCode;
                        state = State.STOPPED;
                        pid = null;
                        startedAt = null;
                        closeStdinQuietly();
                        this.process = null;
                    }
                }
                log.info("Minecraft server process exited with code {}", exitCode);
            } catch (InterruptedException interrupted) {
                Thread.currentThread().interrupt();
            }
        }, "vesta-minecraft-process");
        watcher.setDaemon(true);
        watcher.start();
        return true;
    }

    /** Stops any running process, then starts a fresh one. */
    public synchronized boolean restart() throws IOException {
        stop();
        return start();
    }

    /**
     * Graceful stop: write Minecraft's {@code stop} command to stdin, wait up to
     * {@link #STOP_TIMEOUT}, then force-terminate if needed.
     */
    public synchronized void stop() {
        Process current = process;
        if (current == null || !current.isAlive()) {
            state = State.STOPPED;
            pid = null;
            startedAt = null;
            closeStdinQuietly();
            process = null;
            return;
        }
        state = State.STOPPING;
        try {
            if (stdin != null) {
                stdin.write("stop");
                stdin.newLine();
                stdin.flush();
            }
        } catch (IOException e) {
            log.warn("Could not write stop command to Minecraft stdin: {}", e.getMessage());
        }
        try {
            if (!current.waitFor(STOP_TIMEOUT.toSeconds(), TimeUnit.SECONDS)) {
                log.warn("Minecraft server did not stop in time; terminating it");
                current.destroyForcibly();
                current.waitFor(5, TimeUnit.SECONDS);
            }
        } catch (InterruptedException interrupted) {
            Thread.currentThread().interrupt();
            current.destroyForcibly();
        } finally {
            if (current.isAlive()) {
                current.destroyForcibly();
            }
            lastExitCode = current.isAlive() ? null : current.exitValue();
            state = State.STOPPED;
            pid = null;
            startedAt = null;
            closeStdinQuietly();
            process = null;
        }
    }

    /** Immediate force kill without attempting a graceful stop command. */
    public synchronized void kill() {
        Process current = process;
        if (current == null || !current.isAlive()) {
            state = State.STOPPED;
            pid = null;
            startedAt = null;
            closeStdinQuietly();
            process = null;
            return;
        }
        state = State.STOPPING;
        log.warn("Force-killing Minecraft server process (pid {})", current.pid());
        current.destroyForcibly();
        try {
            current.waitFor(5, TimeUnit.SECONDS);
        } catch (InterruptedException interrupted) {
            Thread.currentThread().interrupt();
        } finally {
            if (current.isAlive()) {
                current.destroyForcibly();
            }
            try {
                lastExitCode = current.exitValue();
            } catch (IllegalThreadStateException ignored) {
                lastExitCode = null;
            }
            state = State.STOPPED;
            pid = null;
            startedAt = null;
            closeStdinQuietly();
            process = null;
        }
    }

    private void closeStdinQuietly() {
        BufferedWriter writer = stdin;
        stdin = null;
        if (writer != null) {
            try {
                writer.close();
            } catch (IOException ignored) {
                // ignore
            }
        }
    }
}
