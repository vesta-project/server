package com.vesta.wrapper.launch;

import java.nio.file.Path;
import java.util.ArrayList;
import java.util.Collections;
import java.util.List;
import java.util.Objects;

/**
 * Immutable, shell-free definition of how one Minecraft-compatible server process is launched: an
 * executable and a literal argument vector. It never invokes a shell and performs no filesystem access;
 * the process layer checks the referenced files immediately before launching.
 *
 * <p>Three shapes exist because servers are not all {@code java -jar server.jar}:
 * <ul>
 *   <li>{@link Kind#JAVA_JAR}: {@code java [jvm args] -jar JAR [args]};</li>
 *   <li>{@link Kind#JAVA_ARGFILE}: {@code java [jvm args] @FILE... [args]}, the layout used by loader
 *       installers that generate argument files;</li>
 *   <li>{@link Kind#NATIVE}: {@code EXECUTABLE [args]} for servers that are not Java programs.</li>
 * </ul>
 * Changing an executable path or a JVM argument that loads code is a trusted local operational capability,
 * not a remote parameter. Values are never echoed in error messages or {@link #toString()}, because
 * arguments may carry secrets.
 *
 * <p>How the server is asked to stop is part of the definition ({@link StopMode}); a definition that
 * cannot be stopped gracefully is never given an invented stop command.
 */
public final class LaunchSpec {
    public static final int MAX_ARGS = 256;
    public static final int MAX_ARG_CHARS = 4_096;
    public static final int MAX_ARGFILES = 16;

    public enum Kind {
        JAVA_JAR("java-jar"),
        JAVA_ARGFILE("java-argfile"),
        NATIVE("native");

        private final String wire;

        Kind(String wire) {
            this.wire = wire;
        }

        public String wireName() {
            return wire;
        }

        /** @throws IllegalArgumentException for any unknown value (no echo) */
        public static Kind fromWire(String value) {
            for (Kind kind : values()) {
                if (kind.wire.equals(value)) {
                    return kind;
                }
            }
            throw new IllegalArgumentException("Unknown launch kind");
        }
    }

    public enum StopMode {
        /** Graceful stop writes the single console line {@code stop}. */
        CONSOLE_STOP("console-stop"),
        /** No graceful stop is known; stop requests are rejected without any effect. */
        UNSUPPORTED("unsupported");

        private final String wire;

        StopMode(String wire) {
            this.wire = wire;
        }

        public String wireName() {
            return wire;
        }

        /** @throws IllegalArgumentException for any unknown value (no echo) */
        public static StopMode fromWire(String value) {
            for (StopMode mode : values()) {
                if (mode.wire.equals(value)) {
                    return mode;
                }
            }
            throw new IllegalArgumentException("Unknown stop mode");
        }
    }

    private final Kind kind;
    private final String javaCommand;
    private final List<String> jvmArgs;
    private final Path artifact;
    private final List<Path> argFiles;
    private final List<String> args;
    private final StopMode stopMode;

    private LaunchSpec(Kind kind, String javaCommand, List<String> jvmArgs, Path artifact, List<Path> argFiles,
                       List<String> args, StopMode stopMode) {
        this.kind = kind;
        this.javaCommand = javaCommand;
        this.jvmArgs = jvmArgs;
        this.artifact = artifact;
        this.argFiles = argFiles;
        this.args = args;
        this.stopMode = stopMode;
    }

    /** Strictly validated {@code java -jar} definition; graceful stop is the console {@code stop} line. */
    public static LaunchSpec javaJar(String javaCommand, List<String> jvmArgs, Path jar, List<String> args) {
        return new LaunchSpec(Kind.JAVA_JAR, text("java command", javaCommand), textList("JVM arguments", jvmArgs),
                path("jar", jar), List.of(), textList("arguments", args), StopMode.CONSOLE_STOP);
    }

    /** Strictly validated {@code java @argfile...} definition; graceful stop is the console {@code stop} line. */
    public static LaunchSpec javaArgFiles(String javaCommand, List<String> jvmArgs, List<Path> argFiles,
                                          List<String> args) {
        if (argFiles == null || argFiles.isEmpty() || argFiles.size() > MAX_ARGFILES) {
            throw bad("argument files");
        }
        List<Path> files = new ArrayList<>();
        for (Path file : argFiles) {
            files.add(path("argument file", file));
        }
        return new LaunchSpec(Kind.JAVA_ARGFILE, text("java command", javaCommand), textList("JVM arguments", jvmArgs),
                null, List.copyOf(files), textList("arguments", args), StopMode.CONSOLE_STOP);
    }

    /** Strictly validated direct-executable definition. The stop mode is mandatory: it is never assumed. */
    public static LaunchSpec nativeExecutable(Path executable, List<String> args, StopMode stopMode) {
        if (stopMode == null) {
            throw bad("stop mode");
        }
        return new LaunchSpec(Kind.NATIVE, null, List.of(), path("executable", executable), List.of(),
                textList("arguments", args), stopMode);
    }

    /**
     * Reproduces the pre-existing legacy {@code minecraft.*} launch configuration exactly, without new
     * validation, so previously accepted configurations keep their behavior (a malformed value still fails at
     * launch, as before). New definitions must use the strict factories.
     */
    public static LaunchSpec legacyJavaJar(String javaCommand, List<String> jvmArgs, Path jar, List<String> args) {
        return new LaunchSpec(Kind.JAVA_JAR, javaCommand, Collections.unmodifiableList(new ArrayList<>(jvmArgs)),
                Objects.requireNonNull(jar, "jar"), List.of(), Collections.unmodifiableList(new ArrayList<>(args)),
                StopMode.CONSOLE_STOP);
    }

    /** Copy with another stop mode (a Java definition may be marked as having no graceful stop). */
    public LaunchSpec withStopMode(StopMode mode) {
        if (mode == null) {
            throw bad("stop mode");
        }
        return new LaunchSpec(kind, javaCommand, jvmArgs, artifact, argFiles, args, mode);
    }

    /** Copy whose relative file paths are resolved against {@code base} and normalized. */
    public LaunchSpec resolveAgainst(Path base) {
        Path root = Objects.requireNonNull(base, "base").toAbsolutePath().normalize();
        List<Path> files = new ArrayList<>();
        for (Path file : argFiles) {
            files.add(resolved(root, file));
        }
        return new LaunchSpec(kind, javaCommand, jvmArgs, artifact == null ? null : resolved(root, artifact),
                List.copyOf(files), args, stopMode);
    }

    private static Path resolved(Path root, Path path) {
        return path.isAbsolute() ? path.normalize() : root.resolve(path).normalize();
    }

    public Kind kind() {
        return kind;
    }

    /** Java executable for the Java kinds; {@code null} for {@link Kind#NATIVE}. */
    public String javaCommand() {
        return javaCommand;
    }

    public List<String> jvmArgs() {
        return jvmArgs;
    }

    /** The JAR ({@link Kind#JAVA_JAR}) or executable ({@link Kind#NATIVE}); {@code null} for argument files. */
    public Path artifact() {
        return artifact;
    }

    public List<Path> argFiles() {
        return argFiles;
    }

    public List<String> args() {
        return args;
    }

    public StopMode stopMode() {
        return stopMode;
    }

    public boolean gracefulStopSupported() {
        return stopMode == StopMode.CONSOLE_STOP;
    }

    /** The literal process command; paths are used as given, so resolve them first. */
    public List<String> command() {
        List<String> command = new ArrayList<>();
        switch (kind) {
            case JAVA_JAR -> {
                command.add(javaCommand);
                command.addAll(jvmArgs);
                command.add("-jar");
                command.add(artifact.toString());
                command.addAll(args);
            }
            case JAVA_ARGFILE -> {
                command.add(javaCommand);
                command.addAll(jvmArgs);
                for (Path file : argFiles) {
                    command.add("@" + file);
                }
                command.addAll(args);
            }
            case NATIVE -> {
                command.add(artifact.toString());
                command.addAll(args);
            }
        }
        return List.copyOf(command);
    }

    @Override
    public boolean equals(Object other) {
        return other instanceof LaunchSpec that && kind == that.kind && Objects.equals(javaCommand, that.javaCommand)
                && jvmArgs.equals(that.jvmArgs) && Objects.equals(artifact, that.artifact)
                && argFiles.equals(that.argFiles) && args.equals(that.args) && stopMode == that.stopMode;
    }

    @Override
    public int hashCode() {
        return Objects.hash(kind, javaCommand, jvmArgs, artifact, argFiles, args, stopMode);
    }

    /** Shape only: values and paths are never printed because arguments may contain secrets. */
    @Override
    public String toString() {
        return "LaunchSpec[kind=" + kind.wireName() + ", jvmArgs=" + jvmArgs.size() + ", argFiles=" + argFiles.size()
                + ", args=" + args.size() + ", stop=" + stopMode.wireName() + "]";
    }

    // ------------------------------------------------------------------ validation (no value echo)

    private static String text(String field, String value) {
        if (value == null || value.isBlank() || value.length() > MAX_ARG_CHARS) {
            throw bad(field);
        }
        for (int i = 0; i < value.length(); i++) {
            char c = value.charAt(i);
            int type = Character.getType(c);
            if (Character.isISOControl(c) || type == Character.LINE_SEPARATOR || type == Character.PARAGRAPH_SEPARATOR) {
                throw bad(field);
            }
        }
        return value;
    }

    private static List<String> textList(String field, List<String> values) {
        if (values == null || values.size() > MAX_ARGS) {
            throw bad(field);
        }
        List<String> out = new ArrayList<>();
        for (String value : values) {
            out.add(text(field, value));
        }
        return List.copyOf(out);
    }

    private static Path path(String field, Path value) {
        if (value == null) {
            throw bad(field);
        }
        text(field, value.toString());
        return value;
    }

    private static IllegalArgumentException bad(String field) {
        return new IllegalArgumentException("Invalid launch " + field);
    }
}
