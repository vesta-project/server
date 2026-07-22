package com.vesta.wrapper.runtime;

import com.vesta.wrapper.console.ConsoleBuffer;

import java.io.IOException;
import java.time.Duration;
import java.util.ArrayList;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import java.util.Optional;

/**
 * Collects online players by issuing Minecraft's {@code list} command and parsing console output.
 */
public final class PlayersQuery {

    private static final Duration DEFAULT_TIMEOUT = Duration.ofSeconds(3);
    private static final long POLL_SLEEP_MS = 50L;

    private final Duration timeout;

    public PlayersQuery() {
        this(DEFAULT_TIMEOUT);
    }

    public PlayersQuery(Duration timeout) {
        this.timeout = timeout != null ? timeout : DEFAULT_TIMEOUT;
    }

    public Map<String, Object> query(MinecraftServerProcess minecraft) {
        if (minecraft == null || !minecraft.isConfigured()) {
            Map<String, Object> out = new LinkedHashMap<>();
            out.put("manageable", false);
            out.put("players", List.of());
            return out;
        }

        MinecraftServerProcess.State state = minecraft.state();
        boolean running = state == MinecraftServerProcess.State.RUNNING && minecraft.isRunning();
        if (!running) {
            Map<String, Object> out = new LinkedHashMap<>();
            out.put("manageable", true);
            out.put("running", false);
            out.put("online", 0);
            out.put("players", List.of());
            return out;
        }

        ConsoleBuffer buffer = minecraft.consoleBuffer();
        long since = buffer.latestCursor();
        try {
            minecraft.sendCommand("list");
        } catch (IOException e) {
            Map<String, Object> out = new LinkedHashMap<>();
            out.put("manageable", true);
            out.put("running", true);
            out.put("online", 0);
            out.put("players", List.of());
            out.put("error", e.getMessage());
            return out;
        }

        Optional<PlayerListParser.Result> parsed = awaitListResult(buffer, since);
        Map<String, Object> out = new LinkedHashMap<>();
        out.put("manageable", true);
        out.put("running", true);
        if (parsed.isPresent()) {
            PlayerListParser.Result result = parsed.get();
            out.put("online", result.online);
            out.put("max", result.max);
            List<Map<String, Object>> players = new ArrayList<>();
            for (PlayerListParser.Player player : result.players) {
                players.add(player.toMap());
            }
            out.put("players", players);
        } else {
            out.put("online", 0);
            out.put("players", List.of());
            out.put("error", "timed out waiting for list response");
        }
        return out;
    }

    private Optional<PlayerListParser.Result> awaitListResult(ConsoleBuffer buffer, long since) {
        long deadline = System.nanoTime() + timeout.toNanos();
        long cursor = since;
        while (System.nanoTime() < deadline) {
            ConsoleBuffer.Snapshot snapshot = buffer.snapshot(cursor, 200);
            for (ConsoleBuffer.Line line : snapshot.lines) {
                Optional<PlayerListParser.Result> parsed = PlayerListParser.parseLine(line.text);
                if (parsed.isPresent()) {
                    return parsed;
                }
            }
            if (!snapshot.lines.isEmpty()) {
                cursor = snapshot.cursor;
            }
            try {
                Thread.sleep(POLL_SLEEP_MS);
            } catch (InterruptedException interrupted) {
                Thread.currentThread().interrupt();
                return Optional.empty();
            }
        }
        return Optional.empty();
    }
}
