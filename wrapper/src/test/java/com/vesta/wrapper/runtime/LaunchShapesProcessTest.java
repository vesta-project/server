package com.vesta.wrapper.runtime;

import com.vesta.wrapper.config.WrapperConfig;
import com.vesta.wrapper.console.ConsoleBuffer;
import com.vesta.wrapper.launch.LaunchSpec;
import com.vesta.wrapper.launch.LaunchSpec.StopMode;
import com.vesta.wrapper.runtime.MinecraftServerProcess.LifecycleException;
import org.junit.jupiter.api.AfterEach;
import org.junit.jupiter.api.BeforeEach;
import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.Assumptions;
import org.junit.jupiter.api.io.TempDir;
import org.junit.jupiter.params.ParameterizedTest;
import org.junit.jupiter.params.provider.ValueSource;

import java.io.IOException;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.time.Duration;
import java.util.ArrayList;
import java.util.List;
import java.util.concurrent.atomic.AtomicInteger;

import static com.vesta.wrapper.runtime.FakeGameFixtures.await;
import static org.junit.jupiter.api.Assertions.*;

/** Real owned fake-game processes through every launch shape; none is a Minecraft or FerrumC certification. */
class LaunchShapesProcessTest {
    private static final List<String> LITERALS = List.of("a b", "x;y", "p&q", "$HOME", "%PATH%", "it's", "<redir", "|pipe", "`tick`",
            "--flag=va lue", "héllo");
    @TempDir Path root;
    private Path work;
    private Path jar;
    private MinecraftServerProcess game;

    @BeforeEach void prepare() throws Exception {
        work = Files.createDirectories(root.resolve("game"));
        jar = FakeGameFixtures.jar(work);
    }

    @AfterEach void cleanup() throws Exception {
        if (game != null) { game.kill(); await(() -> !game.isRunning()); }
        FakeGameFixtures.deleteTreeWithRetry(root);
    }

    private LaunchSpec spec(String shape, String mode, List<String> extra) throws Exception {
        List<String> programArgs = new ArrayList<>(List.of(mode)); programArgs.addAll(extra);
        switch (shape) {
            case "jar":
                return LaunchSpec.javaJar(FakeGameFixtures.javaExecutable().toString(), List.of(), jar, programArgs);
            case "argfile": {
                Files.writeString(work.resolve("launch.args"), "-jar\n\"" + jar.toString().replace('\\', '/') + "\"\n", StandardCharsets.UTF_8);
                return LaunchSpec.javaArgFiles(FakeGameFixtures.javaExecutable().toString(), List.of(), List.of(Path.of("launch.args")), programArgs);
            }
            default: {
                List<String> args = new ArrayList<>(List.of("-jar", jar.toString())); args.addAll(programArgs);
                return LaunchSpec.nativeExecutable(FakeGameFixtures.javaExecutable(), args, StopMode.CONSOLE_STOP);
            }
        }
    }

    private WrapperConfig config;

    private MinecraftServerProcess process(LaunchSpec spec, Duration stopTimeout) {
        config = new WrapperConfig.Builder().scanRoot(work).minecraftWorkingDirectory(work).minecraftLaunch(spec).build();
        game = new MinecraftServerProcess(config, new ConsoleBuffer(), stopTimeout);
        return game;
    }

    private boolean output(String text) {
        return game.consoleBuffer().snapshot(0, 100).lines.stream().anyMatch(line -> line.text.equals(text));
    }

    private String start(MinecraftServerProcess process, AtomicInteger gates) throws Exception {
        String generation = process.startGeneration(gates::incrementAndGet).generationId();
        await(() -> output("FAKE_READY"));
        return generation;
    }

    private List<String> recordedArgs() throws IOException { return Files.readAllLines(work.resolve("args.txt"), StandardCharsets.UTF_8); }

    private int starts() throws IOException {
        Path marker = work.resolve("starts.txt");
        return Files.exists(marker) ? Files.readAllLines(marker).size() : 0;
    }

    // ------------------------------------------------------------------ every shape launches and stops for real

    @ParameterizedTest @ValueSource(strings = {"jar", "argfile", "native"})
    void everyShapeLaunchesARealProcessInTheWorkingDirectoryAndStopsGracefullyByGeneration(String shape) throws Exception {
        var process = process(spec(shape, "graceful", List.of("literal one", "x;y")), Duration.ofSeconds(10));
        var gates = new AtomicInteger();
        String generation = start(process, gates);
        assertTrue(MinecraftServerProcess.GENERATION_ID.matcher(generation).matches()); assertEquals(1, gates.get());
        assertEquals(List.of("graceful", "literal one", "x;y"), recordedArgs());
        assertTrue(Files.isSameFile(Path.of(Files.readString(work.resolve("cwd.txt"), StandardCharsets.UTF_8)), work));
        assertEquals(1, starts()); assertTrue(process.isRunning()); assertEquals(generation, process.currentGenerationId());
        var stopped = process.stopGeneration(generation, gates::incrementAndGet, Duration.ofSeconds(10));
        assertEquals("stopped", stopped.outcome()); assertEquals(0, stopped.exitCode()); assertEquals(2, gates.get());
        assertEquals(MinecraftServerProcess.State.STOPPED, process.state()); assertNull(process.currentGenerationId());
        assertEquals(1, starts());
    }

    @ParameterizedTest @ValueSource(strings = {"jar", "argfile", "native"})
    void argumentsReachTheProcessLiterallyWithoutShellInterpretation(String shape) throws Exception {
        var process = process(spec(shape, "graceful", LITERALS), Duration.ofSeconds(10));
        start(process, new AtomicInteger());
        List<String> expected = new ArrayList<>(List.of("graceful")); expected.addAll(LITERALS);
        assertEquals(expected, recordedArgs());
        assertFalse(Files.exists(work.resolve("pwned"))); // no shell ran a redirect or command
        assertTrue(process.isRunning());
    }

    @ParameterizedTest @ValueSource(strings = {"jar", "argfile", "native"})
    void snapshotAndCommandDescribeTheConfiguredShape(String shape) throws Exception {
        var spec = spec(shape, "graceful", List.of());
        var process = process(spec, Duration.ofSeconds(1));
        var snapshot = process.snapshot();
        assertEquals(true, snapshot.get("configured")); assertEquals(true, snapshot.get("manageable"));
        assertEquals(switch (shape) { case "jar" -> "java-jar"; case "argfile" -> "java-argfile"; default -> "native"; }, snapshot.get("launch_kind"));
        assertEquals(true, snapshot.get("graceful_stop_supported"));
        assertEquals(config.minecraftLaunch.command(), process.command());
        assertEquals(spec.resolveAgainst(work).command(), process.command());
        assertTrue(process.gracefulStopSupported());
    }

    @Test void explicitJavaJarSupportsTheLegacyStartStopRestartMethodsToo() throws Exception {
        var process = process(spec("jar", "graceful", List.of()), Duration.ofSeconds(10));
        assertTrue(process.start()); await(() -> output("FAKE_READY"));
        assertTrue(process.stop()); assertFalse(process.isRunning());
        assertTrue(process.restart());
        // The new JVM records its start a moment after the process exists; wait for that marker.
        await(() -> { try { return starts() == 2; } catch (IOException notYet) { return false; } });
        assertTrue(process.isRunning());
    }

    // ------------------------------------------------------------------ unsupported graceful stop

    @ParameterizedTest @ValueSource(strings = {"native", "jar"})
    void unsupportedStopIsRejectedBeforeAnyGateStateChangeOutputOrKillAndForceKillStillWorks(String shape) throws Exception {
        // Mode "graceful" would EXIT on a written "stop", so a surviving process proves nothing was written.
        var process = process(spec(shape, "graceful", List.of()).withStopMode(StopMode.UNSUPPORTED), Duration.ofSeconds(5));
        var gates = new AtomicInteger();
        String generation = start(process, gates); assertEquals(1, gates.get());
        assertFalse(process.gracefulStopSupported()); assertEquals(false, process.snapshot().get("graceful_stop_supported"));

        var rejected = assertThrows(LifecycleException.class, () -> process.stopGeneration(generation, gates::incrementAndGet, Duration.ofSeconds(5)));
        assertEquals(LifecycleException.GRACEFUL_STOP_UNSUPPORTED, rejected.getCode());
        assertEquals(1, gates.get()); assertEquals(MinecraftServerProcess.State.RUNNING, process.state());
        assertEquals(generation, process.currentGenerationId());

        long begun = System.nanoTime();
        assertFalse(process.stop()); // legacy stop: no wait, no state change
        assertTrue(Duration.ofNanos(System.nanoTime() - begun).toMillis() < 2_000);
        assertEquals(MinecraftServerProcess.State.RUNNING, process.state());
        var restart = assertThrows(IOException.class, process::restart);
        assertTrue(restart.getMessage().contains("graceful stop is not supported"));
        assertEquals(1, starts()); assertTrue(process.isRunning());

        process.sendCommand("list"); await(() -> output("COMMAND:list"));
        assertFalse(output("COMMAND:stop")); assertTrue(process.isRunning());

        var stale = assertThrows(LifecycleException.class, () -> process.stopGeneration("gen_AAAAAAAAAAAAAAAAAAAAAA", gates::incrementAndGet, Duration.ofSeconds(1)));
        assertEquals(LifecycleException.STALE_GENERATION, stale.getCode()); // generation precedence is unchanged

        var killed = process.killGeneration(generation, gates::incrementAndGet, Duration.ofSeconds(10));
        assertEquals(generation, killed.generationId()); assertEquals(2, gates.get());
        await(() -> !process.isRunning());
        assertFalse(output("COMMAND:stop"));
        var again = process.stopGeneration(generation, gates::incrementAndGet, Duration.ofSeconds(1));
        assertEquals("not_running", again.outcome()); assertEquals(2, gates.get());
        String replacement = process.startGeneration(gates::incrementAndGet).generationId();
        assertNotEquals(generation, replacement);
    }

    // ------------------------------------------------------------------ known failures before any effect

    private void assertNoEffect(MinecraftServerProcess process, AtomicInteger gates, String code) throws IOException {
        var failure = assertThrows(LifecycleException.class, () -> process.startGeneration(gates::incrementAndGet));
        assertEquals(code, failure.getCode());
        assertEquals(0, gates.get()); assertFalse(process.isRunning()); assertEquals(MinecraftServerProcess.State.STOPPED, process.state());
        assertNull(process.currentGenerationId()); assertEquals(0, starts());
    }

    @Test void missingPrimaryArtifactsAreKnownNoEffectFailuresForEveryShape() throws Exception {
        Path absent = work.resolve("absent");
        var gates = new AtomicInteger();
        assertNoEffect(process(LaunchSpec.javaJar("java", List.of(), absent.resolve("a.jar"), List.of()), Duration.ofSeconds(1)), gates, LifecycleException.LAUNCH_JAR_MISSING);
        Files.writeString(work.resolve("present.args"), "-version\n");
        assertNoEffect(process(LaunchSpec.javaArgFiles("java", List.of(), List.of(work.resolve("present.args"), absent.resolve("b.args")), List.of()),
                Duration.ofSeconds(1)), gates, LifecycleException.LAUNCH_ARGFILE_MISSING);
        assertNoEffect(process(LaunchSpec.nativeExecutable(absent.resolve("server"), List.of(), StopMode.CONSOLE_STOP), Duration.ofSeconds(1)), gates,
                LifecycleException.LAUNCH_EXECUTABLE_MISSING);
        assertNoEffect(process(LaunchSpec.nativeExecutable(work, List.of(), StopMode.CONSOLE_STOP), Duration.ofSeconds(1)), gates,
                LifecycleException.LAUNCH_EXECUTABLE_MISSING); // a directory is not an executable
    }

    @Test void aNonExecutableFileIsAKnownNoEffectFailureWhereTheFilesystemRecordsExecutePermission() throws Exception {
        Assumptions.assumeFalse(FakeGameFixtures.windows(), "Windows has no execute bit to refuse");
        Path plain = Files.writeString(work.resolve("plain-file"), "not a program");
        assertNoEffect(process(LaunchSpec.nativeExecutable(plain, List.of(), StopMode.CONSOLE_STOP), Duration.ofSeconds(1)), new AtomicInteger(),
                LifecycleException.LAUNCH_EXECUTABLE_NOT_EXECUTABLE);
    }

    @Test void aMissingWorkingDirectoryIsKnownAfterTheArtifactsExist() throws Exception {
        for (String shape : new String[]{"jar", "argfile", "native"}) {
            var spec = spec(shape, "graceful", List.of());
            var config = new WrapperConfig.Builder().scanRoot(work).minecraftWorkingDirectory(root.resolve("no-such-directory")).minecraftLaunch(
                    spec.resolveAgainst(work)).build();
            game = new MinecraftServerProcess(config, new ConsoleBuffer(), Duration.ofSeconds(1));
            assertNoEffect(game, new AtomicInteger(), LifecycleException.WORKING_DIRECTORY_MISSING);
        }
    }

    @Test void noLaunchDefinitionIsNotConfiguredAndNeverGuessesACommand() throws Exception {
        var config = new WrapperConfig.Builder().scanRoot(work).minecraftWorkingDirectory(work).build();
        var process = new MinecraftServerProcess(config, new ConsoleBuffer(), Duration.ofSeconds(1));
        game = process; var gates = new AtomicInteger();
        assertFalse(process.isConfigured()); assertTrue(process.command().isEmpty()); assertFalse(process.gracefulStopSupported());
        assertFalse(process.snapshot().containsKey("launch_kind")); assertEquals(false, process.snapshot().get("manageable"));
        assertEquals(LifecycleException.NOT_CONFIGURED, assertThrows(LifecycleException.class, () -> process.startGeneration(gates::incrementAndGet)).getCode());
        assertThrows(IOException.class, process::start); assertThrows(IOException.class, () -> process.sendCommand("list"));
        assertEquals("not_running", process.stopGeneration("gen_AAAAAAAAAAAAAAAAAAAAAA", gates::incrementAndGet, Duration.ofSeconds(1)).outcome());
        assertEquals(0, gates.get());
    }

    @Test void legacyStartKeepsItsOriginalJarMessageAndAnArgfileFailureIsDescriptive() throws Exception {
        var missingJar = process(LaunchSpec.legacyJavaJar("java", List.of(), work.resolve("gone.jar"), List.of("nogui")), Duration.ofSeconds(1));
        assertTrue(assertThrows(IOException.class, missingJar::start).getMessage().startsWith("Minecraft launch jar does not exist: "));
        var missingArgfile = process(LaunchSpec.javaArgFiles("java", List.of(), List.of(work.resolve("gone.args")), List.of()), Duration.ofSeconds(1));
        assertEquals("A configured argument file does not exist", assertThrows(IOException.class, missingArgfile::start).getMessage());
        assertEquals(0, starts());
    }
}
