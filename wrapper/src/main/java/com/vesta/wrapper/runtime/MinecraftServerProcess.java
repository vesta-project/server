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
import java.security.SecureRandom;
import java.time.Duration;
import java.time.Instant;
import java.util.ArrayList;
import java.util.Base64;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Locale;
import java.util.Map;
import java.util.concurrent.TimeUnit;
import java.util.regex.Pattern;

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
    /**
     * Opaque random id of the live process launched by this wrapper, paired with that exact process so a
     * lock-free reader never reports an id for an exited or replaced process. Set on every actual launch
     * (legacy and typed), cleared when the process is observed exited.
     */
    private volatile LiveGeneration live;

    private record LiveGeneration(Process process, String id) {
    }

    private static final SecureRandom GENERATION_RANDOM = new SecureRandom();
    /** Generation ids: "gen_" + 22 base64url chars (128 random bits). Never derived from the PID. */
    public static final Pattern GENERATION_ID = Pattern.compile("gen_[A-Za-z0-9_-]{22}");

    /**
     * Called by typed lifecycle operations immediately before the real effect, while the process monitor
     * is held (so the preceding validation and the effect cannot be separated by another mutation). It may
     * throw to abort; nothing is then changed. It must not call back into this object's synchronized methods
     * from another thread and wait for them.
     */
    @FunctionalInterface
    public interface EffectGate {
        void beforeEffect();
    }

    /** Known, no-guess lifecycle outcome with a stable code. */
    public static final class LifecycleException extends Exception {
        public static final String NOT_CONFIGURED = "not_configured";
        public static final String LAUNCH_JAR_MISSING = "launch_jar_missing";
        public static final String WORKING_DIRECTORY_MISSING = "working_directory_missing";
        public static final String GAME_STOPPING = "game_stopping";
        public static final String LAUNCH_FAILED = "launch_failed";
        public static final String STALE_GENERATION = "stale_generation";
        public static final String NOT_RUNNING = "not_running";
        public static final String STOP_TIMEOUT = "stop_timeout";
        public static final String KILL_UNCONFIRMED = "kill_unconfirmed";

        private final String code;

        public LifecycleException(String code, String message) {
            super(message, null, false, false);
            this.code = code;
        }

        public String getCode() {
            return code;
        }
    }

    /** @param launched false when a live, not-stopping process already existed (no effect, gate not called) */
    public record StartResult(boolean launched, String generationId) {
    }

    /**
     * @param outcome "stopped" (the expected generation exited) or "not_running" (no live process; no effect)
     */
    public record StopResult(String outcome, String generationId, Integer exitCode) {
    }

    public record KillResult(String generationId, Integer exitCode) {
    }

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

    /** Graceful stop wait used by {@link #stop()} and typed stop operations. */
    public Duration stopTimeout() {
        return stopTimeout;
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
            live = null;
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
        try {
            launchLocked();
        } catch (NotLaunched e) {
            // Legacy contract: rethrow the original launch failure unchanged.
            if (e.getCause() instanceof IOException io) {
                throw io;
            }
            throw (RuntimeException) e.getCause();
        }
        return true;
    }

    /**
     * Opaque id of the live process generation, or null when no process launched by this wrapper is alive.
     * Lock-free (never takes the process monitor), so it is safe to call from inside state-database
     * callbacks; the value can change immediately after it is read, so effects must re-check under
     * {@link #startGeneration}/{@link #stopGeneration}/{@link #killGeneration}.
     */
    public String currentGenerationId() {
        LiveGeneration g = live;
        return g != null && g.process().isAlive() ? g.id() : null;
    }

    /** Generation of the live process; caller holds the monitor. */
    private String liveGenerationLocked() {
        Process current = process;
        LiveGeneration g = live;
        if (current == null || !current.isAlive() || g == null || g.process() != current) {
            return null;
        }
        return g.id();
    }

    /**
     * Typed start. Under the process monitor: if a live process exists returns it without any effect
     * (and without re-validating launch configuration); a live STOPPING process fails {@code game_stopping}.
     * Otherwise validates launch configuration (known no-effect failures), calls {@code gate.beforeEffect()},
     * then launches and assigns a fresh generation id.
     */
    public synchronized StartResult startGeneration(EffectGate gate) throws LifecycleException {
        java.util.Objects.requireNonNull(gate, "gate");
        State current = state();
        if (current == State.STOPPING) {
            throw new LifecycleException(LifecycleException.GAME_STOPPING,
                    "The game server is still stopping; start was not attempted");
        }
        if (current == State.RUNNING || current == State.STARTING) {
            String id = liveGenerationLocked();
            if (id == null) {
                // Live process without a generation cannot happen (every launch assigns one); fail closed.
                throw new LifecycleException(LifecycleException.LAUNCH_FAILED, "Live process has no generation");
            }
            return new StartResult(false, id);
        }
        if (config.minecraftLaunchJar == null) {
            throw new LifecycleException(LifecycleException.NOT_CONFIGURED, "minecraft.launch-jar is not configured");
        }
        if (!Files.isRegularFile(config.minecraftLaunchJar)) {
            throw new LifecycleException(LifecycleException.LAUNCH_JAR_MISSING, "The configured launch jar does not exist");
        }
        if (config.minecraftWorkingDirectory == null || !Files.isDirectory(config.minecraftWorkingDirectory)) {
            throw new LifecycleException(LifecycleException.WORKING_DIRECTORY_MISSING,
                    "The configured working directory does not exist");
        }
        gate.beforeEffect();
        try {
            return new StartResult(true, launchLocked());
        } catch (NotLaunched e) {
            // No process was created, so this is a known failure. Failures after creation are NOT caught
            // here: they propagate as unknown outcomes (recovery_required for this non-idempotent start).
            throw new LifecycleException(LifecycleException.LAUNCH_FAILED,
                    "The game process could not be launched: " + e.getCause().getClass().getSimpleName());
        }
    }

    /**
     * Typed graceful stop of exactly {@code expectedGeneration}. Never kills or restarts.
     * <ul>
     *   <li>No live process: {@code not_running} result, no effect, gate not called.</li>
     *   <li>A different live generation: {@code stale_generation}, no effect.</li>
     *   <li>Expected generation live and not yet stopping: gate, then STOPPING and the console stop line, all
     *       under the monitor. Already STOPPING: no new effect; just waits.</li>
     *   <li>Waits up to {@code timeout} outside the monitor; on timeout throws {@code stop_timeout} and the
     *       process stays live and STOPPING.</li>
     * </ul>
     *
     * @throws InterruptedException if interrupted while waiting (process stays STOPPING)
     */
    public StopResult stopGeneration(String expectedGeneration, EffectGate gate, Duration timeout)
            throws LifecycleException, InterruptedException {
        java.util.Objects.requireNonNull(gate, "gate");
        if (timeout == null || timeout.isNegative()) {
            throw new IllegalArgumentException("timeout must be non-negative");
        }
        Process current;
        synchronized (this) {
            State s = state();
            if (s == State.STOPPED) {
                return new StopResult("not_running", null, null);
            }
            String id = liveGenerationLocked();
            if (id == null || !id.equals(expectedGeneration)) {
                throw new LifecycleException(LifecycleException.STALE_GENERATION,
                        "A different game generation is running; nothing was stopped");
            }
            current = process;
            if (state != State.STOPPING) {
                gate.beforeEffect();
                state = State.STOPPING;
                try {
                    writeLine("stop");
                } catch (IOException e) {
                    log.warn("Could not write stop command to Minecraft stdin: {}", e.getMessage());
                }
            }
        }
        if (!current.waitFor(timeout.toMillis(), TimeUnit.MILLISECONDS)) {
            throw new LifecycleException(LifecycleException.STOP_TIMEOUT,
                    "The game server did not stop within " + timeout.toSeconds() + "s; it remains stopping");
        }
        Integer exit = current.exitValue();
        synchronized (this) {
            if (process == current) {
                clearStopped(current);
            }
        }
        return new StopResult("stopped", expectedGeneration, exit);
    }

    /**
     * Typed force kill of exactly {@code expectedGeneration}. The generation match, {@code gate.beforeEffect()}
     * and {@code destroyForcibly()} happen atomically under the monitor, so a replacement process is never
     * signalled. Waits up to {@code waitForExit} outside the monitor.
     *
     * @throws LifecycleException {@code not_running}/{@code stale_generation} (no effect) or
     *                            {@code kill_unconfirmed} (signal sent, still alive after the wait)
     */
    public KillResult killGeneration(String expectedGeneration, EffectGate gate, Duration waitForExit)
            throws LifecycleException, InterruptedException {
        java.util.Objects.requireNonNull(gate, "gate");
        if (waitForExit == null || waitForExit.isNegative()) {
            throw new IllegalArgumentException("waitForExit must be non-negative");
        }
        Process current;
        synchronized (this) {
            if (state() == State.STOPPED) {
                throw new LifecycleException(LifecycleException.NOT_RUNNING, "No game process is running; nothing was killed");
            }
            String id = liveGenerationLocked();
            if (id == null || !id.equals(expectedGeneration)) {
                throw new LifecycleException(LifecycleException.STALE_GENERATION,
                        "A different game generation is running; nothing was killed");
            }
            current = process;
            gate.beforeEffect();
            state = State.STOPPING;
            log.warn("Force-killing Minecraft server process generation {} (pid {})", id, current.pid());
            current.destroyForcibly();
        }
        if (!current.waitFor(waitForExit.toMillis(), TimeUnit.MILLISECONDS)) {
            throw new LifecycleException(LifecycleException.KILL_UNCONFIRMED,
                    "Kill signal sent but the game process has not exited yet");
        }
        Integer exit = current.exitValue();
        synchronized (this) {
            if (process == current) {
                clearStopped(current);
            }
        }
        return new KillResult(expectedGeneration, exit);
    }

    /**
     * Thrown only when no process was created (preparation or {@code ProcessBuilder.start} failed): a known,
     * no-effect launch failure. Any failure after the process exists propagates as-is (unknown outcome) and
     * the live process stays published.
     */
    private static final class NotLaunched extends Exception {
        NotLaunched(Exception cause) {
            super(null, cause, false, false);
        }
    }

    /** Launches the configured command; caller holds the monitor and has checked nothing is alive. */
    private String launchLocked() throws NotLaunched {
        Process launched;
        String generation;
        try {
            // Everything that can fail without creating a process happens before start().
            generation = newGenerationId();
            state = State.STARTING;
            ProcessBuilder builder = new ProcessBuilder(command());
            builder.directory(config.minecraftWorkingDirectory.toFile());
            builder.redirectErrorStream(true);
            builder.redirectInput(ProcessBuilder.Redirect.PIPE);
            builder.redirectOutput(ProcessBuilder.Redirect.PIPE);
            builder.redirectError(ProcessBuilder.Redirect.PIPE);
            launched = builder.start();
        } catch (IOException | RuntimeException e) {
            clearStopped(null);
            throw new NotLaunched(e);
        }
        // The process exists from here on: publish it with its generation first, so a later setup failure
        // (propagated to the caller as an unknown outcome) never drops or hides a live game.
        process = launched;
        live = new LiveGeneration(launched, generation);
        state = State.RUNNING;
        stdin = new BufferedWriter(new OutputStreamWriter(process.getOutputStream(), StandardCharsets.UTF_8));
        pid = process.pid();
        startedAt = Instant.now();
        lastExitCode = null;
        startOutputReader(launched);
        log.info("Started Minecraft server process (pid {})", pid);

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
                        live = null;
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
        return generation;
    }

    private static String newGenerationId() {
        byte[] raw = new byte[16];
        GENERATION_RANDOM.nextBytes(raw);
        return "gen_" + Base64.getUrlEncoder().withoutPadding().encodeToString(raw);
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
        live = null;
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
            live = null;
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
