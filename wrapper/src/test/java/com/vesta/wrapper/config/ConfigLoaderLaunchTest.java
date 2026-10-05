package com.vesta.wrapper.config;

import com.vesta.wrapper.launch.LaunchSpec;
import com.vesta.wrapper.launch.LaunchSpec.Kind;
import com.vesta.wrapper.launch.LaunchSpec.StopMode;
import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.io.TempDir;
import org.junit.jupiter.params.ParameterizedTest;
import org.junit.jupiter.params.provider.MethodSource;
import org.junit.jupiter.params.provider.ValueSource;

import java.nio.file.Files;
import java.nio.file.Path;
import java.util.List;
import java.util.stream.Stream;

import static org.junit.jupiter.api.Assertions.*;

class ConfigLoaderLaunchTest {
    private static final String SECRET = "TOP-SECRET-MARKER";
    @TempDir Path temp;

    private String workdir() { return "working-directory = \"" + temp.toString().replace("\\", "/") + "\""; }

    private WrapperConfig load(String toml) throws Exception {
        Path file = temp.resolve("wrapper-config.toml");
        Files.writeString(file, toml);
        return ConfigLoader.load(file);
    }

    private void invalid(String toml) throws Exception {
        ConfigLoader.ConfigException failure = assertThrows(ConfigLoader.ConfigException.class, () -> load(toml));
        // Parse failures name the file by design; values must never appear.
        assertFalse(failure.toString().contains(SECRET), failure.getMessage());
        assertNull(failure.getCause());
    }

    // ------------------------------------------------------------------ accepted definitions

    @Test void javaJarDefinitionResolvesAgainstTheWorkingDirectoryWithoutTouchingLegacyFields() throws Exception {
        WrapperConfig config = load("""
                [minecraft]
                auto-start = true
                %s

                [launch]
                kind = "java-jar"
                java-command = "custom-java"
                jvm-args = ["-Xms1G", "-Xmx3G"]
                jar = "fabric-server.jar"
                args = ["nogui", "--demo"]
                """.formatted(workdir()));
        assertTrue(config.minecraftAutoStart);
        assertNull(config.minecraftLaunchJar);
        assertEquals(LaunchSpec.javaJar("custom-java", List.of("-Xms1G", "-Xmx3G"), temp.resolve("fabric-server.jar"), List.of("nogui", "--demo")),
                config.minecraftLaunch);
        assertEquals(List.of("custom-java", "-Xms1G", "-Xmx3G", "-jar", temp.resolve("fabric-server.jar").toString(), "nogui", "--demo"),
                config.minecraftLaunch.command());
    }

    @Test void javaJarMinimalDefinitionUsesExplicitDefaultsAndNeverInventsProgramArguments() throws Exception {
        WrapperConfig config = load("[minecraft]\n" + workdir() + "\n\n[launch]\nkind = \"java-jar\"\njar = \"server.jar\"\n");
        LaunchSpec spec = config.minecraftLaunch;
        assertEquals(Kind.JAVA_JAR, spec.kind()); assertEquals("java", spec.javaCommand());
        assertTrue(spec.jvmArgs().isEmpty()); assertTrue(spec.args().isEmpty()); // no implicit "nogui"
        assertEquals(StopMode.CONSOLE_STOP, spec.stopMode());
        assertEquals(List.of("java", "-jar", temp.resolve("server.jar").toString()), spec.command());
    }

    @Test void javaArgFileDefinitionKeepsFileOrderAndProgramArguments() throws Exception {
        WrapperConfig config = load("""
                [minecraft]
                %s

                [launch]
                kind = "java-argfile"
                jvm-args = ["-Dflag=1"]
                argfiles = ["user_jvm_args.txt", "libraries/net/unix_args.txt"]
                args = ["nogui"]
                stop = "console-stop"
                """.formatted(workdir()));
        LaunchSpec spec = config.minecraftLaunch;
        assertEquals(Kind.JAVA_ARGFILE, spec.kind());
        assertEquals(List.of(temp.resolve("user_jvm_args.txt"), temp.resolve("libraries/net/unix_args.txt")), spec.argFiles());
        assertEquals(List.of("java", "-Dflag=1", "@" + temp.resolve("user_jvm_args.txt"), "@" + temp.resolve("libraries/net/unix_args.txt"), "nogui"),
                spec.command());
    }

    @ParameterizedTest @ValueSource(strings = {"console-stop", "unsupported"})
    void nativeDefinitionRequiresAndHonoursItsExplicitStopMode(String stop) throws Exception {
        WrapperConfig config = load("[minecraft]\n" + workdir() + "\n\n[launch]\nkind = \"native\"\nexecutable = \"bin/server\"\nargs = [\"--world\", \"my world\"]\nstop = \""
                + stop + "\"\n");
        LaunchSpec spec = config.minecraftLaunch;
        assertEquals(Kind.NATIVE, spec.kind()); assertEquals(StopMode.fromWire(stop), spec.stopMode());
        assertEquals(List.of(temp.resolve("bin/server").toString(), "--world", "my world"), spec.command());
        assertNull(spec.javaCommand()); assertTrue(spec.jvmArgs().isEmpty());
    }

    @Test void javaDefinitionsMayDeclareThatThereIsNoGracefulStop() throws Exception {
        WrapperConfig config = load("[minecraft]\n" + workdir() + "\n\n[launch]\nkind = \"java-jar\"\njar = \"s.jar\"\nstop = \"unsupported\"\n");
        assertFalse(config.minecraftLaunch.gracefulStopSupported());
    }

    @Test void absoluteLaunchPathsAreKeptAndNormalized() throws Exception {
        String exe = temp.resolve("elsewhere").resolve("..").resolve("server").toString().replace("\\", "/");
        WrapperConfig config = load("[minecraft]\n" + workdir() + "\n\n[launch]\nkind = \"native\"\nexecutable = \"" + exe + "\"\nstop = \"unsupported\"\n");
        assertEquals(temp.resolve("server"), config.minecraftLaunch.artifact());
    }

    // ------------------------------------------------------------------ legacy behavior

    @Test void legacyMinecraftTableStillProducesTheEquivalentJavaJarDefinition() throws Exception {
        WrapperConfig config = load("""
                [minecraft]
                launch-jar = "fabric-server.jar"
                %s
                java-command = "custom-java"
                jvm-args = ["-Xms1G"]
                server-args = ["nogui", "--demo"]
                """.formatted(workdir()));
        assertEquals(temp.resolve("fabric-server.jar"), config.minecraftLaunchJar);
        assertEquals(LaunchSpec.legacyJavaJar("custom-java", List.of("-Xms1G"), temp.resolve("fabric-server.jar"), List.of("nogui", "--demo")), config.minecraftLaunch);
        assertEquals(StopMode.CONSOLE_STOP, config.minecraftLaunch.stopMode());
    }

    @Test void legacyDefaultServerArgumentsRemainNoguiOnlyForTheLegacyForm() throws Exception {
        WrapperConfig config = load("[minecraft]\nlaunch-jar = \"s.jar\"\n" + workdir() + "\n");
        assertEquals(List.of("nogui"), config.minecraftLaunch.args());
    }

    @Test void missingFileAndNoLaunchConfigurationYieldNoDefinition() throws Exception {
        assertNull(ConfigLoader.load(temp.resolve("missing.toml")).minecraftLaunch);
        assertNull(load("[minecraft]\nauto-start = false\n").minecraftLaunch);
    }

    @ParameterizedTest @ValueSource(strings = {"launch-jar = \"legacy.jar\"", "java-command = \"java\"", "jvm-args = []", "server-args = [\"nogui\"]",
            "launch-jar = \"\"", "java-command = \"TOP-SECRET-MARKER\""})
    void anyLegacyLaunchKeyNextToLaunchFailsClosedInsteadOfPickingOne(String legacy) throws Exception {
        ConfigLoader.ConfigException failure = assertThrows(ConfigLoader.ConfigException.class, () -> load(
                "[minecraft]\n" + legacy + "\n" + workdir() + "\n\n[launch]\nkind = \"java-jar\"\njar = \"s.jar\"\n"));
        assertTrue(failure.getMessage().contains("conflicts")); assertFalse(failure.toString().contains(SECRET));
    }

    // ------------------------------------------------------------------ strict rejection without echo

    static Stream<String> invalidTables() {
        String secretList = "[\"" + SECRET + "\"]";
        return Stream.of(
                "[[launch]]\nkind = \"native\"\nexecutable = \"" + SECRET + "\"\nstop = \"unsupported\"\n",
                "[launch]\n",
                "[launch]\njar = \"" + SECRET + "\"\n",
                "[launch]\nkind = \"docker\"\njar = \"" + SECRET + "\"\n",
                "[launch]\nkind = 5\njar = \"" + SECRET + "\"\n",
                "[launch]\nkind = \"JAVA-JAR\"\njar = \"" + SECRET + "\"\n",
                "[launch]\nkind = \"java-jar\"\njar = \"s.jar\"\nshell = \"" + SECRET + "\"\n",
                "[launch]\nkind = \"java-jar\"\njar = \"s.jar\"\nenv = " + secretList + "\n",
                "[launch]\nkind = \"java-jar\"\njar = \"s.jar\"\n[launch.extra]\nvalue = \"" + SECRET + "\"\n",
                "[launch]\nkind = \"java-jar\"\nexecutable = \"" + SECRET + "\"\n",
                "[launch]\nkind = \"java-jar\"\njar = \"s.jar\"\nargfiles = " + secretList + "\n",
                "[launch]\nkind = \"java-argfile\"\njar = \"" + SECRET + "\"\nargfiles = [\"a.txt\"]\n",
                "[launch]\nkind = \"java-argfile\"\nargfiles = [\"a.txt\"]\nexecutable = \"" + SECRET + "\"\n",
                "[launch]\nkind = \"native\"\njar = \"" + SECRET + "\"\nexecutable = \"x\"\nstop = \"unsupported\"\n",
                "[launch]\nkind = \"native\"\nexecutable = \"x\"\njvm-args = " + secretList + "\nstop = \"unsupported\"\n",
                "[launch]\nkind = \"native\"\nexecutable = \"x\"\njava-command = \"" + SECRET + "\"\nstop = \"unsupported\"\n",
                "[launch]\nkind = \"java-jar\"\njava-command = \"" + SECRET + "\"\n",
                "[launch]\nkind = \"java-argfile\"\njvm-args = " + secretList + "\n",
                "[launch]\nkind = \"native\"\nargs = " + secretList + "\nstop = \"unsupported\"\n",
                "[launch]\nkind = \"native\"\nexecutable = \"" + SECRET + "\"\n",
                "[launch]\nkind = \"native\"\nexecutable = \"x\"\nargs = " + secretList + "\nstop = \"kill\"\n",
                "[launch]\nkind = \"native\"\nexecutable = \"x\"\nargs = " + secretList + "\nstop = true\n",
                "[launch]\nkind = \"java-jar\"\njar = \"s.jar\"\nstop = \"" + SECRET + "\"\n",
                "[launch]\nkind = \"java-jar\"\njar = 5\njvm-args = " + secretList + "\n",
                "[launch]\nkind = \"java-jar\"\njar = \"s.jar\"\njava-command = 5\nargs = " + secretList + "\n",
                "[launch]\nkind = \"java-jar\"\njar = \"s.jar\"\njvm-args = \"" + SECRET + "\"\n",
                "[launch]\nkind = \"java-jar\"\njar = \"s.jar\"\nargs = [1, \"" + SECRET + "\"]\n",
                "[launch]\nkind = \"java-argfile\"\nargfiles = \"" + SECRET + "\"\n",
                "[launch]\nkind = \"java-argfile\"\nargfiles = [5, \"" + SECRET + "\"]\n",
                "[launch]\nkind = \"java-argfile\"\nargfiles = []\nargs = " + secretList + "\n",
                "[launch]\nkind = \"java-argfile\"\nargs = " + secretList + "\n",
                "[launch]\nkind = \"java-jar\"\njar = \"\"\nargs = " + secretList + "\n",
                "[launch]\nkind = \"java-jar\"\njar = \"s.jar\"\njava-command = \"\"\nargs = " + secretList + "\n",
                "[launch]\nkind = \"java-jar\"\njar = \"s.jar\"\nargs = [\"\"]\njvm-args = " + secretList + "\n",
                "[launch]\nkind = \"java-jar\"\njar = \"s.jar\"\nargs = [\"" + SECRET + "\\nstop\"]\n",
                "[launch]\nkind = \"java-jar\"\njar = \"s.jar\"\njvm-args = [\"" + SECRET + "\\u0000\"]\n",
                "[launch]\nkind = \"native\"\nexecutable = \"" + SECRET + "\\n\"\nstop = \"unsupported\"\n",
                "[launch]\nkind = \"java-jar\"\njar = \"s.jar\"\nargs = [" + "\"a\", ".repeat(LaunchSpec.MAX_ARGS) + "\"" + SECRET + "\"]\n",
                "[launch]\nkind = \"java-jar\"\njar = \"s.jar\"\nargs = [\"" + "x".repeat(LaunchSpec.MAX_ARG_CHARS) + SECRET + "\"]\n");
    }

    @ParameterizedTest @MethodSource("invalidTables")
    void everyInvalidLaunchShapeFailsClosedWithoutEchoingValuesOrFallingBackToLegacyDefaults(String launch) throws Exception {
        invalid("[minecraft]\n" + workdir() + "\n\n" + launch);
    }

    @Test void aLaunchKeyAtTheTopLevelOfTheFileIsNotATableAndFailsClosed() throws Exception {
        invalid("launch = \"" + SECRET + "\"\n[minecraft]\n" + workdir() + "\n");
    }

    @Test void anInvalidLaunchTableIsNeverSilentlyReplacedByTheLegacyForm() throws Exception {
        // Legacy keys elsewhere do not rescue an invalid [launch]: the conflict itself already refuses to start.
        invalid("[minecraft]\nlaunch-jar = \"legacy.jar\"\n" + workdir() + "\n\n[launch]\nkind = \"docker\"\n");
    }

    // ------------------------------------------------------------------ builder semantics

    @Test void builderRejectsAnExplicitDefinitionTogetherWithTheLegacyLaunchJar() {
        var spec = LaunchSpec.javaJar("java", List.of(), temp.resolve("a.jar"), List.of());
        assertThrows(IllegalStateException.class, () -> new WrapperConfig.Builder().scanRoot(temp)
                .minecraftLaunchJar(temp.resolve("legacy.jar")).minecraftLaunch(spec).build());
    }

    @Test void builderResolvesAnExplicitDefinitionAgainstTheWorkingDirectoryAndKeepsLegacyFieldsEmpty() {
        Path work = temp.resolve("work");
        var config = new WrapperConfig.Builder().scanRoot(temp).minecraftWorkingDirectory(work)
                .minecraftLaunch(LaunchSpec.nativeExecutable(Path.of("bin", "server"), List.of("go"), StopMode.UNSUPPORTED)).build();
        assertEquals(work.toAbsolutePath().normalize().resolve("bin").resolve("server"), config.minecraftLaunch.artifact());
        assertNull(config.minecraftLaunchJar);
        assertEquals(List.of(config.minecraftLaunch.artifact().toString(), "go"), config.minecraftLaunch.command());
    }

    @Test void builderLegacyFormMatchesThePreviousCommandExactly() {
        Path root = temp.toAbsolutePath();
        var config = new WrapperConfig.Builder().scanRoot(root).minecraftWorkingDirectory(root).minecraftLaunchJar(Path.of("fabric-server.jar"))
                .minecraftJavaCommand("custom-java").minecraftJvmArgs(List.of("-Xms1G", "-Xmx3G")).minecraftServerArgs(List.of("nogui")).build();
        assertEquals(List.of("custom-java", "-Xms1G", "-Xmx3G", "-jar", root.resolve("fabric-server.jar").toString(), "nogui"), config.minecraftLaunch.command());
        assertNull(new WrapperConfig.Builder().scanRoot(root).build().minecraftLaunch);
    }
}
