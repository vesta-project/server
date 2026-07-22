package com.vesta.wrapper.runtime;

import java.util.ArrayList;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Locale;
import java.util.Map;
import java.util.Optional;
import java.util.regex.Matcher;
import java.util.regex.Pattern;

/**
 * Parses vanilla/Paper {@code list} command console output.
 *
 * <p>Examples:
 * <ul>
 *   <li>{@code There are 0 of a max of 20 players online:}</li>
 *   <li>{@code There are 2 of a max of 20 players online: Steve, Alex}</li>
 *   <li>{@code There are 1/20 players online: Steve}</li>
 * </ul>
 */
public final class PlayerListParser {

    public static final class Player {
        public final String name;
        public final String uuid;

        public Player(String name, String uuid) {
            this.name = name;
            this.uuid = uuid;
        }

        public Map<String, Object> toMap() {
            Map<String, Object> out = new LinkedHashMap<>();
            out.put("name", name);
            if (uuid != null && !uuid.isBlank()) {
                out.put("uuid", uuid);
            }
            return out;
        }
    }

    public static final class Result {
        public final int online;
        public final int max;
        public final List<Player> players;

        public Result(int online, int max, List<Player> players) {
            this.online = online;
            this.max = max;
            this.players = List.copyOf(players);
        }
    }

    private static final Pattern OF_A_MAX = Pattern.compile(
            "(?i)There are\\s+(\\d+)\\s+of a max of\\s+(\\d+)\\s+players online:\\s*(.*)$"
    );
    private static final Pattern SLASH = Pattern.compile(
            "(?i)There are\\s+(\\d+)\\s*/\\s*(\\d+)\\s+players online:\\s*(.*)$"
    );
    private static final Pattern UUID = Pattern.compile(
            "(?i)^([0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12})$"
    );
    private static final Pattern NAME_WITH_UUID = Pattern.compile(
            "(?i)^(.+?)\\s*\\(([0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12})\\)$"
    );

    private PlayerListParser() {}

    public static Optional<Result> parseLine(String rawLine) {
        if (rawLine == null || rawLine.isBlank()) {
            return Optional.empty();
        }
        String line = stripPrefixes(stripFormatting(rawLine)).trim();
        Matcher ofMax = OF_A_MAX.matcher(line);
        if (ofMax.find()) {
            return Optional.of(build(
                    Integer.parseInt(ofMax.group(1)),
                    Integer.parseInt(ofMax.group(2)),
                    ofMax.group(3)
            ));
        }
        Matcher slash = SLASH.matcher(line);
        if (slash.find()) {
            return Optional.of(build(
                    Integer.parseInt(slash.group(1)),
                    Integer.parseInt(slash.group(2)),
                    slash.group(3)
            ));
        }
        return Optional.empty();
    }

    private static Result build(int online, int max, String namesPart) {
        List<Player> players = parseNames(namesPart);
        int effectiveOnline = online;
        if (!players.isEmpty() && online == 0) {
            effectiveOnline = players.size();
        }
        return new Result(effectiveOnline, max, players);
    }

    private static List<Player> parseNames(String namesPart) {
        List<Player> players = new ArrayList<>();
        if (namesPart == null || namesPart.isBlank()) {
            return players;
        }
        for (String token : namesPart.split(",")) {
            String raw = token.trim();
            if (raw.isEmpty()) {
                continue;
            }
            Matcher withUuid = NAME_WITH_UUID.matcher(raw);
            if (withUuid.matches()) {
                players.add(new Player(withUuid.group(1).trim(), withUuid.group(2).toLowerCase(Locale.ROOT)));
                continue;
            }
            if (UUID.matcher(raw).matches()) {
                // Bare UUID without a name — skip rather than invent a display name.
                continue;
            }
            players.add(new Player(raw, null));
        }
        return players;
    }

    private static String stripFormatting(String text) {
        // Minecraft legacy color codes + ANSI CSI sequences from some wrappers.
        String withoutSection = text.replaceAll("§.", "");
        return withoutSection.replaceAll("\\u001B\\[[0-9;]*[A-Za-z]", "");
    }

    private static String stripPrefixes(String text) {
        String line = text;
        // Common log prefixes: [timestamp] [Server thread/INFO]: message
        int infoIdx = line.indexOf("]: ");
        if (infoIdx >= 0 && infoIdx + 3 < line.length()) {
            String maybe = line.substring(infoIdx + 3).trim();
            if (maybe.toLowerCase(Locale.ROOT).startsWith("there are")) {
                return maybe;
            }
        }
        return line;
    }
}
