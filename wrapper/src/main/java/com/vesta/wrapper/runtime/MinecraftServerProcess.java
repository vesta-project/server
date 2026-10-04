package com.vesta.wrapper.runtime;

import com.vesta.wrapper.config.WrapperConfig;
import com.vesta.wrapper.console.ConsoleBuffer;
import org.slf4j.Logger;
import org.slf4j.LoggerFactory;

import java.io.BufferedReader;
import java.io.BufferedWriter;
import java.io.IOException;
import java.io.InputStreamReader;
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
    public static final Duration DEFAULT_STOP_TIMEOUT = Duration.ofSeconds(10);
    /** Upper bound for one console line; generous enough for long tellraw/JSON commands. */
    public static final int MAX_COMMAND_LENGTH = 4096;

    public enum State {
        STOPPED,
        STARTING,
        RUNNING,
        STOPPING
    }

    private final WrapperConfig config;
    private final ConsoleBuffer consoleBuffer;
    private final Duration stopTimeout;
    private volatile Process process;
    private volatile BufferedWriter stdin;
    private volatile Thread outputReader;
    private volatile State state = State.STOPPED;
    private volatile Long pid;
    private volatile Instant startedAt;
    private volatile Integer lastExitCode;

    public MinecraftServerProcess(WrapperConfig config) {
        this(config, new ConsoleBuffer());
    }

    public MinecraftServerProcess(WrapperConfig config, ConsoleBuffer consoleBuffer) {
        this(config, consoleBuffer, DEFAULT_STOP_TIMEOUT);
    }

    /** @param stopTimeout how long {@link #stop()} waits for a graceful exit before reporting still-stopping. */
    public MinecraftServerProcess(WrapperConfig config, ConsoleBuffer consoleBuffer, Duration stopTimeout) {
        this.config = config;
        this.consoleBuffer = consoleBuffer != null ? consoleBuffer : new ConsoleBuffer();
        if (stopTimeout == null || stopTimeout.isNegative()) {
            throw new IllegalArgumentException("stopTimeout must be non-negative");
        }
        this.stopTimeout = stopTimeout;
    }

    public ConsoleBuffer consoleBuffer() {
        return consoleBuffer;
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
            outputReader = null;
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
        builder.redirectInput(ProcessBuilder.Redirect.PIPE);
        builder.redirectOutput(ProcessBuilder.Redirect.PIPE);
        builder.redirectError(ProcessBuilder.Redirect.PIPE);
        try {
            process = builder.start();
        } catch (IOException | RuntimeException e) {
            clearStopped(null);
            throw e;
        }
        stdin = new BufferedWriter(new OutputStreamWriter(process.getOutputStream(), StandardCharsets.UTF_8));
        pid = process.pid();
        startedAt = Instant.now();
        lastExitCode = null;
        state = State.RUNNING;
        startOutputReader(process);
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
                        outputReader = null;
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

    /**
     * Writes one console line. Rejects CR/LF/NUL and every other control character so a single
     * request can never inject additional console commands, and caps the length.
     */
    public synchronized void sendCommand(String command) throws IOException {
        if (!isConfigured()) {
            throw new IOException("minecraft.launch-jar is not configured");
        }
        validateCommand(command);
        writeLine(command);
    }

    /** Throws {@link IllegalArgumentException} unless {@code command} is a single safe console line. */
    public static void validateCommand(String command) {
        if (command == null || command.isBlank()) {
            throw new IllegalArgumentException("command must not be blank");
        }
        if (command.length() > MAX_COMMAND_LENGTH) {
            throw new IllegalArgumentException("command exceeds " + MAX_COMMAND_LENGTH + " characters");
        }
        for (int i = 0; i < command.length(); i++) {
            char c = command.charAt(i);
            int type = Character.getType(c);
            if (Character.isISOControl(c) || type == Character.LINE_SEPARATOR || type == Character.PARAGRAPH_SEPARATOR) {
                throw new IllegalArgumentException("command must not contain control characters or line breaks");
            }
        }
    }

    private void writeLine(String line) throws IOException {
        if (!isRunning()) {
            throw new IOException("minecraft server is not running");
        }
        BufferedWriter writer = stdin;
        if (writer == null) {
            throw new IOException("minecraft stdin is not available");
        }
        writer.write(line);
        writer.newLine();
        writer.flush();
    }

    /**
     * Gracefully stops any running process, then starts a fresh one.
     *
     * @throws IOException if the previous process has not exited within the stop timeout; no new
     *                     process is launched in that case and the old one remains {@code STOPPING}.
     */
    public boolean restart() throws IOException {
        if (!stop()) {
            throw new IOException("previous Minecraft process is still stopping; restart not started");
        }
        return start();
    }

    /**
     * Graceful stop: enters {@code STOPPING}, writes Minecraft's {@code stop} command to stdin, and waits
     * up to the configured stop timeout. This never force-terminates: on timeout or interrupt the live
     * process, its stdin, and the {@code STOPPING} state are retained so the operator can wait or issue
     * an explicit {@link #kill()}. The wait happens outside the monitor so {@code kill()}, status, and
     * console commands remain usable while shutdown is pending.
     *
     * @return {@code true} once no process is alive; {@code false} if it is still stopping.
     */
    public boolean stop() {
        Process current;
        synchronized (this) {
            current = process;
            if (current == null || !current.isAlive()) {
                clearStopped(current);
                return true;
            }
            if (state != State.STOPPING) {
                state = State.STOPPING;
                try {
                    writeLine("stop");
                } catch (IOException e) {
                    log.warn("Could not write stop command to Minecraft stdin: {}", e.getMessage());
                }
            }
        }
        boolean exited;
        try {
            exited = current.waitFor(stopTimeout.toMillis(), TimeUnit.MILLISECONDS);
        } catch (InterruptedException interrupted) {
            Thread.currentThread().interrupt();
            log.warn("Interrupted while waiting for Minecraft to stop; it remains in STOPPING");
            return false;
        }
        if (!exited) {
            log.warn("Minecraft server did not stop within {}; leaving it STOPPING (use kill to force)",
                    stopTimeout);
            return false;
        }
        synchronized (this) {
            if (process == current) {
                clearStopped(current);
            }
        }
        return true;
    }

    private void clearStopped(Process exited) {
        if (exited != null && !exited.isAlive()) {
            lastExitCode = exited.exitValue();
        }
        state = State.STOPPED;
        pid = null;
        startedAt = null;
        closeStdinQuietly();
        process = null;
        outputReader = null;
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
            outputReader = null;
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
            if (!current.isAlive()) {
                clearStopped(current);
            } else {
                log.warn("Minecraft force kill has not completed; retaining process in STOPPING");
            }
        }
    }

    private void startOutputReader(Process launched) {
        Thread reader = new Thread(() -> {
            try (BufferedReader input = new BufferedReader(
                    new InputStreamReader(launched.getInputStream(), StandardCharsets.UTF_8))) {
                StringBuilder line = new StringBuilder();
                char[] chunk = new char[4096];
                boolean previousCr = false;
                int count;
                while ((count = input.read(chunk)) != -1) {
                    for (int i = 0; i < count; i++) {
                        char ch = chunk[i];
                        if (ch == '\r' || ch == '\n') {
                            if (ch != '\n' || !previousCr) {
                                consoleBuffer.append("stdout", line.toString());
                                line.setLength(0);
                            }
                            previousCr = ch == '\r';
                        } else {
                            previousCr = false;
                            // Discard the rest of an oversized line until its delimiter. Never allocate it.
                            if (line.length() < ConsoleBuffer.MAX_LINE_CHARS) line.append(ch);
                        }
                    }
                }
                if (!line.isEmpty()) consoleBuffer.append("stdout", line.toString());
            } catch (IOException error) {
                if (launched.isAlive()) {
                    log.debug("Stopped reading Minecraft output: {}", error.getMessage());
                }
            }
        }, "vesta-minecraft-output");
        reader.setDaemon(true);
        reader.start();
        outputReader = reader;
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
