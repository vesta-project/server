package com.vesta.wrapper.runtime;

import java.io.BufferedReader;
import java.io.InputStreamReader;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.nio.file.StandardOpenOption;

/** Standalone subprocess fixture; no Minecraft install, network, or real world directory. */
public final class FakeMinecraft {
    private FakeMinecraft() { }

    public static void main(String[] args) throws Exception {
        String mode = args.length == 0 ? "graceful" : args[0];
        // Record exactly what the process received and where it runs (overwritten on every launch).
        Files.write(Path.of("args.txt"), java.util.Arrays.asList(args), StandardCharsets.UTF_8);
        Files.writeString(Path.of("cwd.txt"), Path.of("").toAbsolutePath().toString(), StandardCharsets.UTF_8);
        Files.writeString(Path.of("starts.txt"), ProcessHandle.current().pid() + "\n", StandardOpenOption.CREATE, StandardOpenOption.APPEND);
        System.out.println("FAKE_READY");
        System.out.flush();
        try (var input = new BufferedReader(new InputStreamReader(System.in, StandardCharsets.UTF_8))) {
            String line;
            while ((line = input.readLine()) != null) {
                System.out.println("COMMAND:" + line);
                System.out.flush();
                if (line.equals("crash")) System.exit(23);
                if (line.equals("flood")) {
                    System.out.print("x".repeat(200_000));
                    System.out.print("\r\nAFTER_FLOOD\r\n");
                    System.out.flush();
                }
                if (line.equals("save-all flush")) {
                    System.out.println("FAKE_FLUSH_ACK");
                    System.out.flush();
                }
                if (line.equals("stop") && !mode.equals("ignore-stop")) {
                    if (mode.equals("slow-stop")) Thread.sleep(1200);
                    return;
                }
            }
        }
    }
}
