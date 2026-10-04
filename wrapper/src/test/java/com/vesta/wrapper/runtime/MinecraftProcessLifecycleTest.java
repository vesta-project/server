package com.vesta.wrapper.runtime;

import com.vesta.wrapper.config.WrapperConfig;
import com.vesta.wrapper.console.ConsoleBuffer;
import org.junit.jupiter.api.AfterEach;
import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.io.TempDir;
import org.junit.jupiter.params.ParameterizedTest;
import org.junit.jupiter.params.provider.ValueSource;

import java.io.IOException;
import java.nio.file.Files;
import java.nio.file.Path;
import java.time.Duration;
import java.util.List;
import java.util.concurrent.atomic.AtomicBoolean;
import java.util.concurrent.atomic.AtomicReference;
import java.util.function.BooleanSupplier;
import java.util.jar.Attributes;
import java.util.jar.JarEntry;
import java.util.jar.JarOutputStream;
import java.util.jar.Manifest;

import static org.junit.jupiter.api.Assertions.*;

class MinecraftProcessLifecycleTest {
    @TempDir Path root;
    private MinecraftServerProcess game;
    private Long childPid;

    private MinecraftServerProcess launch(String mode, Duration timeout) throws Exception {
        Path jar = root.resolve("fake-game.jar");
        Manifest manifest = new Manifest();
        manifest.getMainAttributes().put(Attributes.Name.MANIFEST_VERSION, "1.0");
        manifest.getMainAttributes().put(Attributes.Name.MAIN_CLASS, FakeMinecraft.class.getName());
        String classPath = FakeMinecraft.class.getName().replace('.', '/') + ".class";
        try (var output = new JarOutputStream(Files.newOutputStream(jar), manifest);
             var classBytes = FakeMinecraft.class.getClassLoader().getResourceAsStream(classPath)) {
            assertNotNull(classBytes);
            output.putNextEntry(new JarEntry(classPath));
            classBytes.transferTo(output);
            output.closeEntry();
        }
        String executable = System.getProperty("os.name").toLowerCase().contains("windows") ? "java.exe" : "java";
        var config = new WrapperConfig.Builder().scanRoot(root).minecraftWorkingDirectory(root)
                .minecraftJavaCommand(Path.of(System.getProperty("java.home"), "bin", executable).toString())
                .minecraftLaunchJar(jar).minecraftJvmArgs(List.of()).minecraftServerArgs(List.of(mode)).build();
        game = new MinecraftServerProcess(config, new ConsoleBuffer(), timeout);
        assertTrue(game.start());
        childPid = ((Number) game.snapshot().get("pid")).longValue();
        await(() -> hasOutput("FAKE_READY"));
        return game;
    }

    @AfterEach void cleanupChildren() throws Exception {
        if (game != null) game.kill();
        if (childPid != null) {
            var handle = ProcessHandle.of(childPid);
            if (handle.isPresent() && handle.get().isAlive()) {
                handle.get().destroyForcibly();
                handle.get().onExit().get(5, java.util.concurrent.TimeUnit.SECONDS);
            }
        }
        // Windows may signal child exit before all executable/archive handles become deletable.
        Path jar = root.resolve("fake-game.jar");
        long deadline = System.nanoTime() + Duration.ofSeconds(5).toNanos();
        while (Files.exists(jar)) {
            try { Files.delete(jar); }
            catch (java.nio.file.FileSystemException locked) {
                if (System.nanoTime() >= deadline) throw locked;
                Thread.sleep(20);
            }
        }
    }

    @Test
    void timeoutNeverForceKillsAndRestartNeverDuplicatesStillStoppingGame() throws Exception {
        var game = launch("ignore-stop", Duration.ofMillis(80));
        long pid = childPid;
        assertFalse(game.stop());
        assertTrue(ProcessHandle.of(pid).orElseThrow().isAlive());
        assertEquals(MinecraftServerProcess.State.STOPPING, game.state());
        assertThrows(IOException.class, game::restart);
        assertFalse(game.start());
        assertEquals(pid, ((Number) game.snapshot().get("pid")).longValue());
        game.sendCommand("list");
        await(() -> hasOutput("COMMAND:list"));
        assertEquals(1, Files.readAllLines(root.resolve("starts.txt")).size());
        game.kill();
        await(() -> ProcessHandle.of(pid).map(h -> !h.isAlive()).orElse(true));
        assertEquals(MinecraftServerProcess.State.STOPPED, game.state());
    }

    @Test
    void slowGracefulExitCanFinishAfterTimeoutWithoutKill() throws Exception {
        var game = launch("slow-stop", Duration.ofMillis(60));
        assertFalse(game.stop());
        assertTrue(game.isRunning());
        await(() -> !game.isRunning());
        assertEquals(MinecraftServerProcess.State.STOPPED, game.state());
        assertEquals(0, game.snapshot().get("last_exit_code"));
        assertEquals(1, Files.readAllLines(root.resolve("starts.txt")).size());
    }

    @Test
    void interruptedStopRetainsProcessAndInterruptFlag() throws Exception {
        var game = launch("ignore-stop", Duration.ofSeconds(30));
        var failure = new AtomicReference<Throwable>();
        var interrupted = new AtomicBoolean();
        Thread stopper = new Thread(() -> {
            try {
                assertFalse(game.stop());
                interrupted.set(Thread.currentThread().isInterrupted());
            } catch (Throwable e) { failure.set(e); }
        });
        stopper.start();
        try {
            await(() -> hasOutput("COMMAND:stop"));
            stopper.interrupt();
            stopper.join(3000);
            assertFalse(stopper.isAlive());
            if (failure.get() != null) throw new AssertionError(failure.get());
            assertTrue(interrupted.get());
            assertTrue(game.isRunning());
            assertEquals(MinecraftServerProcess.State.STOPPING, game.state());
        } finally {
            stopper.interrupt();
            game.kill();
            stopper.join(3000);
        }
    }

    @Test
    void gracefulStopAndCrashExposeActualExitResults() throws Exception {
        var game = launch("graceful", Duration.ofSeconds(5));
        game.sendCommand("save-all flush");
        await(() -> hasOutput("FAKE_FLUSH_ACK"));
        assertTrue(game.stop());
        assertEquals(0, game.snapshot().get("last_exit_code"));
        assertTrue(game.start());
        childPid = ((Number) game.snapshot().get("pid")).longValue();
        game.sendCommand("crash");
        await(() -> game.snapshot().get("last_exit_code") != null);
        assertEquals(23, game.snapshot().get("last_exit_code"));
        assertFalse(game.isRunning());
    }

    @Test
    void oversizedGameOutputIsBoundedAndFollowingLineStillCaptured() throws Exception {
        var game = launch("ignore-stop", Duration.ofMillis(80));
        game.sendCommand("flood");
        await(() -> hasOutput("AFTER_FLOOD"));
        var lines = game.consoleBuffer().snapshot(0, 100).lines;
        assertTrue(lines.stream().allMatch(line -> line.text.length() <= ConsoleBuffer.MAX_LINE_CHARS));
        assertEquals(1, lines.stream().filter(line -> line.text.startsWith("xxxx")).count());
        assertEquals(ConsoleBuffer.MAX_LINE_CHARS, lines.stream().filter(line -> line.text.startsWith("xxxx")).findFirst().orElseThrow().text.length());
    }

    @Test
    void invalidConsoleRequestHasNoSubprocessEffect() throws Exception {
        var game = launch("ignore-stop", Duration.ofMillis(80));
        assertThrows(IllegalArgumentException.class, () -> game.sendCommand("list\nstop"));
        game.sendCommand("echo-marker");
        await(() -> hasOutput("COMMAND:echo-marker"));
        assertFalse(hasOutput("COMMAND:list"));
        assertFalse(hasOutput("COMMAND:stop"));
        assertTrue(game.isRunning());
    }

    @ParameterizedTest
    @ValueSource(strings = {"", "  ", "list\nstop", "list\rstop", "list\u0000stop", "list\tstop", "list stop", "list stop", "list\u007fstop"})
    void consoleLineValidatorRejectsControlsAndBlankText(String value) {
        assertThrows(IllegalArgumentException.class, () -> MinecraftServerProcess.validateCommand(value));
    }

    @Test
    void consoleLineValidatorHasExplicitLengthBoundary() {
        assertDoesNotThrow(() -> MinecraftServerProcess.validateCommand("x".repeat(MinecraftServerProcess.MAX_COMMAND_LENGTH)));
        assertThrows(IllegalArgumentException.class, () -> MinecraftServerProcess.validateCommand("x".repeat(MinecraftServerProcess.MAX_COMMAND_LENGTH + 1)));
        assertThrows(IllegalArgumentException.class, () -> MinecraftServerProcess.validateCommand(null));
    }

    private boolean hasOutput(String text) {
        return game.consoleBuffer().snapshot(0, 100).lines.stream().anyMatch(line -> line.text.equals(text));
    }

    private static void await(BooleanSupplier condition) throws Exception {
        long deadline = System.nanoTime() + Duration.ofSeconds(8).toNanos();
        while (!condition.getAsBoolean() && System.nanoTime() < deadline) Thread.sleep(10);
        assertTrue(condition.getAsBoolean(), "Subprocess condition did not become true within 8s");
    }
}
