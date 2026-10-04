package com.vesta.wrapper.testutil;

import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.concurrent.TimeUnit;

import static org.junit.jupiter.api.Assumptions.assumeTrue;

public final class NativeTestLinks {
    private NativeTestLinks() { }

    public static void directory(Path link, Path target) throws Exception {
        if (System.getProperty("os.name").toLowerCase().contains("windows")) {
            Process command = new ProcessBuilder("cmd.exe", "/c", "mklink", "/J", link.toString(), target.toString())
                    .redirectErrorStream(true).start();
            boolean exited = command.waitFor(5, TimeUnit.SECONDS);
            if (!exited) {
                command.destroyForcibly();
                command.waitFor(5, TimeUnit.SECONDS);
                assumeTrue(false, "Junction creation timed out");
                return;
            }
            String output = new String(command.getInputStream().readAllBytes(), StandardCharsets.UTF_8);
            assumeTrue(command.exitValue() == 0, "Junction unavailable: " + output);
        } else {
            Files.createSymbolicLink(link, target);
        }
    }
}
