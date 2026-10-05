package com.vesta.wrapper.launch;

import com.vesta.wrapper.launch.LaunchSpec.Kind;
import com.vesta.wrapper.launch.LaunchSpec.StopMode;
import org.junit.jupiter.api.Test;
import org.junit.jupiter.params.ParameterizedTest;
import org.junit.jupiter.params.provider.ValueSource;

import java.nio.file.Path;
import java.util.ArrayList;
import java.util.Collections;
import java.util.List;

import static org.junit.jupiter.api.Assertions.*;

class LaunchSpecTest {
    private static final String SECRET = "TOP-SECRET-MARKER";
    private static final Path ROOT = Path.of("server-root").toAbsolutePath();

    private static void rejected(org.junit.jupiter.api.function.Executable build) {
        IllegalArgumentException failure = assertThrows(IllegalArgumentException.class, build);
        assertTrue(failure.getMessage().startsWith("Invalid launch "));
        assertFalse(failure.toString().contains(SECRET));
        assertNull(failure.getCause());
    }

    // ------------------------------------------------------------------ command shapes

    @Test void javaJarCommandIsExactAndNeverUsesAShell() {
        var spec = LaunchSpec.javaJar("custom-java", List.of("-Xms1G", "-Xmx3G"), ROOT.resolve("fabric-server.jar"), List.of("nogui", "--demo"));
        assertEquals(Kind.JAVA_JAR, spec.kind());
        assertEquals(List.of("custom-java", "-Xms1G", "-Xmx3G", "-jar", ROOT.resolve("fabric-server.jar").toString(), "nogui", "--demo"), spec.command());
        assertEquals(StopMode.CONSOLE_STOP, spec.stopMode()); assertTrue(spec.gracefulStopSupported());
        assertTrue(spec.argFiles().isEmpty());
    }

    @Test void javaArgFileCommandPlacesAtFilesAfterJvmArgumentsAndBeforeProgramArguments() {
        Path user = ROOT.resolve("user_jvm_args.txt"); Path libs = ROOT.resolve("libraries").resolve("unix_args.txt");
        var spec = LaunchSpec.javaArgFiles("java", List.of("-Dflag=1"), List.of(user, libs), List.of("nogui"));
        assertEquals(Kind.JAVA_ARGFILE, spec.kind());
        assertEquals(List.of("java", "-Dflag=1", "@" + user, "@" + libs, "nogui"), spec.command());
        assertNull(spec.artifact()); assertEquals(List.of(user, libs), spec.argFiles());
    }

    @Test void nativeCommandIsTheExecutableAndLiteralArgumentsOnly() {
        Path exe = ROOT.resolve("bin").resolve("server");
        var spec = LaunchSpec.nativeExecutable(exe, List.of("--world", "my world", "run"), StopMode.UNSUPPORTED);
        assertEquals(Kind.NATIVE, spec.kind()); assertNull(spec.javaCommand()); assertTrue(spec.jvmArgs().isEmpty());
        assertEquals(List.of(exe.toString(), "--world", "my world", "run"), spec.command());
        assertFalse(spec.gracefulStopSupported()); assertEquals(exe, spec.artifact());
    }

    @Test void nativeDefinitionsMustStateTheirStopModeBecauseItIsNeverAssumed() {
        rejected(() -> LaunchSpec.nativeExecutable(ROOT.resolve("server"), List.of(), null));
        assertTrue(LaunchSpec.nativeExecutable(ROOT.resolve("server"), List.of(), StopMode.CONSOLE_STOP).gracefulStopSupported());
    }

    @Test void stopModeCanBeChangedOnACopyAndNeverMutatesTheOriginal() {
        var original = LaunchSpec.javaJar("java", List.of(), ROOT.resolve("a.jar"), List.of());
        var changed = original.withStopMode(StopMode.UNSUPPORTED);
        assertTrue(original.gracefulStopSupported()); assertFalse(changed.gracefulStopSupported());
        assertEquals(original.command(), changed.command());
        rejected(() -> original.withStopMode(null));
    }

    @Test void definitionsAreImmutableAndCopyTheirInputs() {
        List<String> jvm = new ArrayList<>(List.of("-Xmx1G")); List<String> args = new ArrayList<>(List.of("nogui"));
        List<Path> files = new ArrayList<>(List.of(ROOT.resolve("args.txt")));
        var jar = LaunchSpec.javaJar("java", jvm, ROOT.resolve("a.jar"), args);
        var argfile = LaunchSpec.javaArgFiles("java", jvm, files, args);
        jvm.add("-Dinjected=1"); args.add("injected"); files.add(ROOT.resolve("injected.txt"));
        assertEquals(List.of("java", "-Xmx1G", "-jar", ROOT.resolve("a.jar").toString(), "nogui"), jar.command());
        assertEquals(List.of("java", "-Xmx1G", "@" + ROOT.resolve("args.txt"), "nogui"), argfile.command());
        assertThrows(UnsupportedOperationException.class, () -> jar.jvmArgs().add("x"));
        assertThrows(UnsupportedOperationException.class, () -> jar.args().add("x"));
        assertThrows(UnsupportedOperationException.class, () -> argfile.argFiles().add(ROOT));
        assertThrows(UnsupportedOperationException.class, () -> jar.command().add("x"));
    }

    // ------------------------------------------------------------------ strict validation, no value echo

    @ParameterizedTest @ValueSource(strings = {"", "   ", "TOP-SECRET-MARKER\n", "TOP-SECRET-MARKER\r", "TOP-SECRET-MARKER\u0000",
            "TOP-SECRET-MARKER\t", "TOP-SECRET-MARKER\u0085", "TOP-SECRET-MARKER\u2028", "TOP-SECRET-MARKER\u2029", "TOP-SECRET-MARKER\u007f"})
    void invalidTextIsRejectedEverywhereWithoutEchoingTheValue(String value) {
        rejected(() -> LaunchSpec.javaJar(value, List.of(), ROOT.resolve("a.jar"), List.of()));
        rejected(() -> LaunchSpec.javaJar("java", List.of(value), ROOT.resolve("a.jar"), List.of()));
        rejected(() -> LaunchSpec.javaJar("java", List.of(), ROOT.resolve("a.jar"), List.of(value)));
        rejected(() -> LaunchSpec.javaArgFiles(value, List.of(), List.of(ROOT.resolve("a.txt")), List.of()));
        rejected(() -> LaunchSpec.javaArgFiles("java", List.of(value), List.of(ROOT.resolve("a.txt")), List.of()));
        rejected(() -> LaunchSpec.nativeExecutable(ROOT.resolve("server"), List.of(value), StopMode.CONSOLE_STOP));
    }

    @Test void controlCharactersInPathsAreRejectedWithoutEcho() {
        for (String name : new String[]{"TOP-SECRET-MARKER\n.jar", "TOP-SECRET-MARKER\u0000.jar"}) {
            try {
                Path bad = Path.of(name);
                rejected(() -> LaunchSpec.javaJar("java", List.of(), bad, List.of()));
                rejected(() -> LaunchSpec.javaArgFiles("java", List.of(), List.of(bad), List.of()));
                rejected(() -> LaunchSpec.nativeExecutable(bad, List.of(), StopMode.CONSOLE_STOP));
            } catch (java.nio.file.InvalidPathException unrepresentable) {
                // The platform refuses to construct the path at all, which is at least as strict.
            }
        }
    }

    @Test void nullAndMalformedCollectionsAndPathsAreRejected() {
        rejected(() -> LaunchSpec.javaJar(null, List.of(), ROOT.resolve("a.jar"), List.of()));
        rejected(() -> LaunchSpec.javaJar("java", null, ROOT.resolve("a.jar"), List.of()));
        rejected(() -> LaunchSpec.javaJar("java", List.of(), null, List.of()));
        rejected(() -> LaunchSpec.javaJar("java", List.of(), ROOT.resolve("a.jar"), null));
        rejected(() -> LaunchSpec.javaJar("java", List.of(), Path.of(""), List.of()));
        rejected(() -> LaunchSpec.javaJar("java", Collections.singletonList(null), ROOT.resolve("a.jar"), List.of()));
        rejected(() -> LaunchSpec.javaArgFiles("java", List.of(), null, List.of()));
        rejected(() -> LaunchSpec.javaArgFiles("java", List.of(), List.of(), List.of()));
        rejected(() -> LaunchSpec.javaArgFiles("java", List.of(), Collections.singletonList(null), List.of()));
        rejected(() -> LaunchSpec.nativeExecutable(null, List.of(), StopMode.CONSOLE_STOP));
        rejected(() -> LaunchSpec.nativeExecutable(ROOT.resolve("server"), null, StopMode.CONSOLE_STOP));
    }

    @Test void explicitBoundariesAreAcceptedAndOneOverIsRejected() {
        String longest = "x".repeat(LaunchSpec.MAX_ARG_CHARS);
        assertDoesNotThrow(() -> LaunchSpec.javaJar("java", List.of(), ROOT.resolve("a.jar"), List.of(longest)));
        rejected(() -> LaunchSpec.javaJar("java", List.of(), ROOT.resolve("a.jar"), List.of(longest + "x")));
        rejected(() -> LaunchSpec.javaJar(longest + "x", List.of(), ROOT.resolve("a.jar"), List.of()));
        List<String> most = Collections.nCopies(LaunchSpec.MAX_ARGS, "a");
        assertDoesNotThrow(() -> LaunchSpec.javaJar("java", most, ROOT.resolve("a.jar"), most));
        List<String> tooMany = Collections.nCopies(LaunchSpec.MAX_ARGS + 1, "a");
        rejected(() -> LaunchSpec.javaJar("java", tooMany, ROOT.resolve("a.jar"), List.of()));
        rejected(() -> LaunchSpec.javaJar("java", List.of(), ROOT.resolve("a.jar"), tooMany));
        rejected(() -> LaunchSpec.nativeExecutable(ROOT.resolve("s"), tooMany, StopMode.CONSOLE_STOP));
        List<Path> files = new ArrayList<>();
        for (int i = 0; i < LaunchSpec.MAX_ARGFILES; i++) files.add(ROOT.resolve("f" + i + ".txt"));
        assertDoesNotThrow(() -> LaunchSpec.javaArgFiles("java", List.of(), files, List.of()));
        List<Path> overflow = new ArrayList<>(files); overflow.add(ROOT.resolve("one-more.txt"));
        rejected(() -> LaunchSpec.javaArgFiles("java", List.of(), overflow, List.of()));
    }

    @Test void argumentsAreLiteralTextNotShellSyntax() {
        List<String> literal = List.of("a b", "x;y", "p&q", "$HOME", "%PATH%", "it's", "<redir", "|pipe", "`tick`", "--flag=va lue", "héllo");
        var spec = LaunchSpec.nativeExecutable(ROOT.resolve("server"), literal, StopMode.CONSOLE_STOP);
        assertEquals(literal, spec.command().subList(1, spec.command().size()));
    }

    // ------------------------------------------------------------------ legacy compatibility

    @Test void legacyDefinitionProducesTheSameCommandAsTheStrictFactoryForOrdinaryInput() {
        Path jar = ROOT.resolve("fabric-server.jar");
        var legacy = LaunchSpec.legacyJavaJar("custom-java", List.of("-Xms1G"), jar, List.of("nogui"));
        assertEquals(LaunchSpec.javaJar("custom-java", List.of("-Xms1G"), jar, List.of("nogui")).command(), legacy.command());
        assertEquals(StopMode.CONSOLE_STOP, legacy.stopMode()); assertEquals(Kind.JAVA_JAR, legacy.kind());
    }

    @Test void legacyDefinitionDoesNotNewlyRejectPreviouslyAcceptedValuesAndCopiesItsInputs() {
        List<String> jvm = new ArrayList<>(List.of("")); List<String> args = new ArrayList<>(List.of("bad\narg"));
        var legacy = assertDoesNotThrow(() -> LaunchSpec.legacyJavaJar("", jvm, ROOT.resolve("a.jar"), args));
        jvm.add("late"); args.add("late");
        assertEquals(List.of("", "", "-jar", ROOT.resolve("a.jar").toString(), "bad\narg"), legacy.command());
        assertThrows(NullPointerException.class, () -> LaunchSpec.legacyJavaJar("java", List.of(), null, List.of()));
    }

    // ------------------------------------------------------------------ path resolution

    @Test void relativePathsResolveAgainstTheBaseAndAbsolutePathsOnlyNormalize() {
        Path absolute = ROOT.resolve("elsewhere").resolve("..").resolve("abs.jar");
        var jar = LaunchSpec.javaJar("java", List.of(), Path.of("sub", "..", "relative.jar"), List.of("nogui")).resolveAgainst(ROOT);
        assertEquals(ROOT.resolve("relative.jar"), jar.artifact());
        assertEquals(ROOT.resolve("abs.jar"), LaunchSpec.javaJar("java", List.of(), absolute, List.of()).resolveAgainst(ROOT.resolve("other")).artifact());
        var argfile = LaunchSpec.javaArgFiles("java", List.of("-Xmx1G"), List.of(Path.of("a.txt"), absolute), List.of("x")).resolveAgainst(ROOT);
        assertEquals(List.of(ROOT.resolve("a.txt"), ROOT.resolve("abs.jar")), argfile.argFiles());
        assertEquals(List.of("java", "-Xmx1G", "@" + ROOT.resolve("a.txt"), "@" + ROOT.resolve("abs.jar"), "x"), argfile.command());
        var exe = LaunchSpec.nativeExecutable(Path.of("bin", "server"), List.of("go"), StopMode.UNSUPPORTED).resolveAgainst(ROOT);
        assertEquals(ROOT.resolve("bin").resolve("server"), exe.artifact()); assertEquals(StopMode.UNSUPPORTED, exe.stopMode());
        assertEquals(List.of("go"), exe.args());
    }

    @Test void resolvingNeverMutatesTheOriginalAndRelativeBaseBecomesAbsolute() {
        var original = LaunchSpec.nativeExecutable(Path.of("bin", "server"), List.of(), StopMode.CONSOLE_STOP);
        var resolved = original.resolveAgainst(Path.of("relative-base"));
        assertEquals(Path.of("bin", "server"), original.artifact());
        assertTrue(resolved.artifact().isAbsolute());
        assertThrows(NullPointerException.class, () -> original.resolveAgainst(null));
    }

    // ------------------------------------------------------------------ wire names, equality, redaction

    @Test void wireNamesRoundTripAndUnknownValuesAreRejectedWithoutEcho() {
        for (Kind kind : Kind.values()) assertEquals(kind, Kind.fromWire(kind.wireName()));
        for (StopMode mode : StopMode.values()) assertEquals(mode, StopMode.fromWire(mode.wireName()));
        assertEquals(List.of("java-jar", "java-argfile", "native"), List.of(Kind.values()).stream().map(Kind::wireName).toList());
        assertEquals(List.of("console-stop", "unsupported"), List.of(StopMode.values()).stream().map(StopMode::wireName).toList());
        for (String bad : new String[]{"JAVA-JAR", "java_jar", "", SECRET}) {
            var failure = assertThrows(IllegalArgumentException.class, () -> Kind.fromWire(bad));
            assertFalse(failure.getMessage().contains(SECRET));
        }
        assertThrows(IllegalArgumentException.class, () -> Kind.fromWire(null));
        for (String bad : new String[]{"CONSOLE-STOP", "stop", "", SECRET}) {
            var failure = assertThrows(IllegalArgumentException.class, () -> StopMode.fromWire(bad));
            assertFalse(failure.getMessage().contains(SECRET));
        }
        assertThrows(IllegalArgumentException.class, () -> StopMode.fromWire(null));
    }

    @Test void equalityCoversEveryFieldAndToStringNeverPrintsValuesOrPaths() {
        Path jar = ROOT.resolve("a.jar");
        var base = LaunchSpec.javaJar("java", List.of("-Dpassword=" + SECRET), jar, List.of("--token=" + SECRET));
        assertEquals(base, LaunchSpec.javaJar("java", List.of("-Dpassword=" + SECRET), jar, List.of("--token=" + SECRET)));
        assertEquals(base.hashCode(), LaunchSpec.javaJar("java", List.of("-Dpassword=" + SECRET), jar, List.of("--token=" + SECRET)).hashCode());
        assertNotEquals(base, LaunchSpec.javaJar("java2", List.of("-Dpassword=" + SECRET), jar, List.of("--token=" + SECRET)));
        assertNotEquals(base, LaunchSpec.javaJar("java", List.of(), jar, List.of("--token=" + SECRET)));
        assertNotEquals(base, LaunchSpec.javaJar("java", List.of("-Dpassword=" + SECRET), ROOT.resolve("b.jar"), List.of("--token=" + SECRET)));
        assertNotEquals(base, LaunchSpec.javaJar("java", List.of("-Dpassword=" + SECRET), jar, List.of()));
        assertNotEquals(base, base.withStopMode(StopMode.UNSUPPORTED));
        assertNotEquals(base, "not a spec");
        String rendered = base + " " + LaunchSpec.nativeExecutable(ROOT.resolve("private-binary"), List.of(SECRET), StopMode.UNSUPPORTED);
        assertFalse(rendered.contains(SECRET)); assertFalse(rendered.contains("private-binary")); assertFalse(rendered.contains("a.jar"));
        assertTrue(rendered.contains("kind=java-jar")); assertTrue(rendered.contains("stop=unsupported"));
    }
}
