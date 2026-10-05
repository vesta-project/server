package com.vesta.wrapper.runtime;

import java.nio.file.Files;
import java.nio.file.Path;
import java.time.Duration;
import java.util.Comparator;
import java.util.function.BooleanSupplier;
import java.util.jar.Attributes;
import java.util.jar.JarEntry;
import java.util.jar.JarOutputStream;
import java.util.jar.Manifest;

import static org.junit.jupiter.api.Assertions.assertNotNull;
import static org.junit.jupiter.api.Assertions.assertTrue;

/** Shared helpers for tests that launch the real {@link FakeMinecraft} fixture JVM; no Minecraft is involved. */
public final class FakeGameFixtures {
    private FakeGameFixtures() { }

    public static boolean windows() {
        return System.getProperty("os.name").toLowerCase().contains("windows");
    }

    /** Absolute path of the JVM running the tests, usable as a Java command or as a direct "native" executable. */
    public static Path javaExecutable() {
        return Path.of(System.getProperty("java.home"), "bin", windows() ? "java.exe" : "java");
    }

    /** Writes a runnable JAR whose main class is {@link FakeMinecraft}. */
    public static Path jar(Path directory) throws Exception {
        Path jar = directory.resolve("fake-game.jar");
        Manifest manifest = new Manifest();
        manifest.getMainAttributes().put(Attributes.Name.MANIFEST_VERSION, "1.0");
        manifest.getMainAttributes().put(Attributes.Name.MAIN_CLASS, FakeMinecraft.class.getName());
        String resource = FakeMinecraft.class.getName().replace('.', '/') + ".class";
        try (var output = new JarOutputStream(Files.newOutputStream(jar), manifest);
             var bytes = FakeMinecraft.class.getClassLoader().getResourceAsStream(resource)) {
            assertNotNull(bytes);
            output.putNextEntry(new JarEntry(resource));
            bytes.transferTo(output);
            output.closeEntry();
        }
        return jar;
    }

    public static void await(BooleanSupplier condition) throws Exception {
        long deadline = System.nanoTime() + Duration.ofSeconds(15).toNanos();
        while (!condition.getAsBoolean() && System.nanoTime() < deadline) Thread.sleep(10);
        assertTrue(condition.getAsBoolean(), "Condition did not complete within fifteen seconds");
    }

    /** Windows can hold an exited child's directory handle briefly; retry, leaving leftovers to @TempDir. */
    public static void deleteTreeWithRetry(Path dir) throws Exception {
        long deadline = System.nanoTime() + Duration.ofSeconds(10).toNanos();
        while (Files.exists(dir)) {
            try (var walk = Files.walk(dir)) {
                walk.sorted(Comparator.reverseOrder()).forEach(path -> path.toFile().delete());
            }
            if (!Files.exists(dir) || System.nanoTime() >= deadline) return;
            Thread.sleep(50);
        }
    }
}
