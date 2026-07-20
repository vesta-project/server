package com.vesta.wrapper.config;

import com.vesta.sync.auth.AccessCapability;
import com.vesta.sync.auth.AccessControlConfig;
import com.vesta.sync.auth.CapabilityEngine;
import com.vesta.sync.auth.SecurityMode;
import com.vesta.wrapper.auth.AuthService;
import org.slf4j.Logger;
import org.slf4j.LoggerFactory;

import java.io.IOException;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.nio.file.StandardCopyOption;
import java.util.ArrayList;
import java.util.Collections;
import java.util.LinkedHashMap;
import java.util.LinkedHashSet;
import java.util.List;
import java.util.Locale;
import java.util.Map;
import java.util.Objects;
import java.util.Set;
import java.util.UUID;
import java.util.regex.Matcher;
import java.util.regex.Pattern;

/**
 * Live security settings for the local admin panel: validate, persist to the
 * active TOML, then hot-apply to {@link AuthService} / {@link CapabilityEngine}.
 */
public final class SecuritySettingsService {

    private static final Logger log = LoggerFactory.getLogger(SecuritySettingsService.class);

    private static final Pattern SECURITY_TABLE = Pattern.compile(
            "(?ms)^\\[security\\]\\s*\\R(.*?)(?=^\\[|\\z)"
    );
    private static final Pattern SECURITY_PEOPLE = Pattern.compile(
            "(?ms)^\\[security\\.people\\]\\s*\\R(.*?)(?=^\\[|\\z)"
    );
    private static final Pattern SECURITY_PERM_NODES = Pattern.compile(
            "(?ms)^\\[security\\.perm-nodes\\]\\s*\\R(.*?)(?=^\\[|\\z)"
    );
    private static final Pattern SECURITY_LEVELS = Pattern.compile(
            "(?ms)^\\[security\\.levels\\]\\s*\\R(.*?)(?=^\\[|\\z)"
    );

    private final Path configFile;
    private final AuthService auth;
    private final CapabilityEngine capabilities;
    private final int controlPort;
    private final Object lock = new Object();

    public SecuritySettingsService(
            Path configFile,
            AuthService auth,
            CapabilityEngine capabilities,
            int controlPort
    ) {
        this.configFile = Objects.requireNonNull(configFile, "configFile").toAbsolutePath().normalize();
        this.auth = Objects.requireNonNull(auth, "auth");
        this.capabilities = Objects.requireNonNull(capabilities, "capabilities");
        this.controlPort = controlPort > 0 ? controlPort : 9091;
    }

    public Path configFile() {
        return configFile;
    }

    /** Snapshot suitable for JSON (password redacted). */
    public Map<String, Object> snapshot() {
        AccessControlConfig access = capabilities.config();
        Map<String, Object> out = new LinkedHashMap<>();
        out.put("mode", auth.mode().name().toLowerCase(Locale.ROOT));
        out.put("password_configured", auth.hasPassword());
        out.put("token_ttl_seconds", auth.tokenTtlSeconds());
        out.put("anonymous_level", access.anonymousBaseline);
        out.put("authenticated_level", access.authenticatedBaseline);
        out.put("password_level", access.passwordBaseline);
        out.put("linked_level", access.linkedBaseline);
        out.put("whitelist_level", access.whitelistBaseline);
        out.put("access_requires_auth", access.accessRequiresAuth);

        Map<String, Object> levels = new LinkedHashMap<>();
        for (int i = 0; i <= 4; i++) {
            levels.put(String.valueOf(i), new ArrayList<>(access.capabilitiesFor(i)));
        }
        out.put("levels", levels);

        List<Map<String, Object>> people = new ArrayList<>();
        for (Map.Entry<UUID, Integer> entry : access.people.entrySet()) {
            Map<String, Object> row = new LinkedHashMap<>();
            row.put("uuid", entry.getKey().toString());
            row.put("level", entry.getValue());
            people.add(row);
        }
        out.put("people", people);

        Map<String, Object> nodes = new LinkedHashMap<>();
        for (Map.Entry<String, Integer> entry : access.permNodes.entrySet()) {
            nodes.put(entry.getKey(), entry.getValue());
        }
        out.put("perm_nodes", nodes);
        out.put("private_paths", new ArrayList<>(access.privatePaths));
        return out;
    }

    /**
     * Validate, persist, then apply. {@code body} keys are snake_case JSON fields.
     * Password is only updated when {@code password} is present (non-null in the map)
     * or {@code clear_password} is true.
     */
    public Map<String, Object> update(Map<String, Object> body) throws IOException {
        Objects.requireNonNull(body, "body");
        synchronized (lock) {
            AccessControlConfig current = capabilities.config();
            SecurityMode mode = body.containsKey("mode")
                    ? SecurityMode.parse(stringValue(body.get("mode")))
                    : auth.mode();

            boolean clearPassword = Boolean.TRUE.equals(body.get("clear_password"));
            boolean passwordPresent = body.containsKey("password");
            String nextPassword = auth.currentPassword();
            boolean passwordChanged = false;
            if (clearPassword) {
                nextPassword = "";
                passwordChanged = true;
            } else if (passwordPresent) {
                String candidate = stringValue(body.get("password"));
                if (candidate == null) {
                    candidate = "";
                }
                nextPassword = candidate;
                passwordChanged = !nextPassword.equals(auth.currentPassword());
            }

            if (mode == SecurityMode.PASSWORD && (nextPassword == null || nextPassword.isBlank())) {
                throw new IllegalArgumentException(
                        "security.mode=password requires a non-empty password"
                );
            }

            long ttl = body.containsKey("token_ttl_seconds")
                    ? longValue(body.get("token_ttl_seconds"), auth.tokenTtlSeconds())
                    : auth.tokenTtlSeconds();
            if (ttl < 60) {
                throw new IllegalArgumentException("token_ttl_seconds must be >= 60");
            }

            int anonymous = body.containsKey("anonymous_level")
                    ? clampLevel(intValue(body.get("anonymous_level"), current.anonymousBaseline))
                    : current.anonymousBaseline;
            int authenticated = body.containsKey("authenticated_level")
                    ? clampLevel(intValue(body.get("authenticated_level"), current.authenticatedBaseline))
                    : current.authenticatedBaseline;
            int passwordLevel = body.containsKey("password_level")
                    ? clampLevel(intValue(body.get("password_level"), current.passwordBaseline))
                    : current.passwordBaseline;
            int linked = body.containsKey("linked_level")
                    ? clampLevel(intValue(body.get("linked_level"), current.linkedBaseline))
                    : current.linkedBaseline;
            int whitelist = body.containsKey("whitelist_level")
                    ? clampLevel(intValue(body.get("whitelist_level"), current.whitelistBaseline))
                    : current.whitelistBaseline;

            Map<Integer, Set<String>> levels = parseLevels(body.get("levels"), current.levels);
            Map<UUID, Integer> people = parsePeople(body.get("people"), current.people);
            Map<String, Integer> permNodes = parsePermNodes(body.get("perm_nodes"), current.permNodes);
            List<String> privatePaths = body.containsKey("private_paths")
                    ? parseStringList(body.get("private_paths"))
                    : new ArrayList<>(current.privatePaths);

            AccessControlConfig nextAccess = new AccessControlConfig(
                    levels,
                    anonymous,
                    authenticated,
                    passwordLevel,
                    linked,
                    whitelist,
                    people,
                    permNodes,
                    privatePaths
            );

            persist(mode, nextPassword, ttl, nextAccess);
            boolean revokeTokens = passwordChanged
                    || mode != auth.mode()
                    || ttl != auth.tokenTtlSeconds();
            auth.applySecurity(mode, nextPassword, ttl, revokeTokens);
            capabilities.setConfig(nextAccess);
            log.info("Applied live security settings (mode={}, people={}, auth_requires={})",
                    mode.name().toLowerCase(Locale.ROOT),
                    people.size(),
                    nextAccess.accessRequiresAuth);
            return snapshot();
        }
    }

    /**
     * Convenience: set/remove a single player grant and persist.
     * Pass {@code level=null} to remove.
     */
    public Map<String, Object> upsertPerson(UUID uuid, Integer level) throws IOException {
        Objects.requireNonNull(uuid, "uuid");
        synchronized (lock) {
            AccessControlConfig current = capabilities.config();
            Map<UUID, Integer> people = new LinkedHashMap<>(current.people);
            if (level == null) {
                people.remove(uuid);
            } else {
                people.put(uuid, clampLevel(level));
            }
            Map<String, Object> body = new LinkedHashMap<>();
            List<Map<String, Object>> rows = new ArrayList<>();
            for (Map.Entry<UUID, Integer> entry : people.entrySet()) {
                Map<String, Object> row = new LinkedHashMap<>();
                row.put("uuid", entry.getKey().toString());
                row.put("level", entry.getValue());
                rows.add(row);
            }
            body.put("people", rows);
            return update(body);
        }
    }

    private void persist(
            SecurityMode mode,
            String password,
            long ttl,
            AccessControlConfig access
    ) throws IOException {
        String original = Files.isRegularFile(configFile)
                ? Files.readString(configFile, StandardCharsets.UTF_8)
                : "";
        String next = rewriteSecuritySections(original, mode, password, ttl, controlPort, access);
        Path parent = configFile.getParent();
        if (parent != null) {
            Files.createDirectories(parent);
        }
        Path temp = configFile.resolveSibling(configFile.getFileName() + ".tmp");
        Files.writeString(temp, next, StandardCharsets.UTF_8);
        try {
            Files.move(temp, configFile, StandardCopyOption.REPLACE_EXISTING, StandardCopyOption.ATOMIC_MOVE);
        } catch (IOException atomicFailed) {
            Files.move(temp, configFile, StandardCopyOption.REPLACE_EXISTING);
        }
    }

    static String rewriteSecuritySections(
            String original,
            SecurityMode mode,
            String password,
            long ttl,
            int controlPort,
            AccessControlConfig access
    ) {
        String text = original == null ? "" : original;
        String securityBody = buildSecurityBody(mode, password, ttl, controlPort, access);
        String peopleBody = buildPeopleBody(access.people);
        String permBody = buildPermNodesBody(access.permNodes);
        String levelsBody = buildLevelsBody(access);

        text = replaceOrAppendTable(text, SECURITY_TABLE, "[security]", securityBody);
        text = replaceOrAppendTable(text, SECURITY_PEOPLE, "[security.people]", peopleBody);
        text = replaceOrAppendTable(text, SECURITY_PERM_NODES, "[security.perm-nodes]", permBody);
        text = replaceOrAppendTable(text, SECURITY_LEVELS, "[security.levels]", levelsBody);
        // Drop legacy [[security.people.entry]] arrays so map form is authoritative.
        text = text.replaceAll("(?ms)^\\[\\[security\\.people\\.entry\\]\\]\\s*\\R(?:^(?!\\[).*\\R)*", "");
        return text.endsWith("\n") ? text : text + "\n";
    }

    private static String replaceOrAppendTable(
            String text,
            Pattern pattern,
            String header,
            String body
    ) {
        String block = header + "\n" + body;
        if (block.endsWith("\n")) {
            // keep single trailing newline between tables
        } else {
            block = block + "\n";
        }
        Matcher matcher = pattern.matcher(text);
        if (matcher.find()) {
            return matcher.replaceFirst(Matcher.quoteReplacement(block + "\n"));
        }
        String base = text.trim().isEmpty() ? "" : text.trim() + "\n\n";
        return base + block + "\n";
    }

    private static String buildSecurityBody(
            SecurityMode mode,
            String password,
            long ttl,
            int controlPort,
            AccessControlConfig access
    ) {
        StringBuilder sb = new StringBuilder();
        sb.append("mode = \"").append(mode.name().toLowerCase(Locale.ROOT)).append("\"\n");
        sb.append("password = \"").append(escapeTomlString(password != null ? password : "")).append("\"\n");
        sb.append("token-ttl-seconds = ").append(ttl).append("\n");
        sb.append("control-port = ").append(controlPort).append("\n");
        sb.append("anonymous-level = ").append(access.anonymousBaseline).append("\n");
        sb.append("authenticated-level = ").append(access.authenticatedBaseline).append("\n");
        sb.append("password-level = ").append(access.passwordBaseline).append("\n");
        sb.append("linked-level = ").append(access.linkedBaseline).append("\n");
        sb.append("whitelist-level = ").append(access.whitelistBaseline).append("\n");
        if (!access.privatePaths.isEmpty()) {
            sb.append("private-paths = [");
            for (int i = 0; i < access.privatePaths.size(); i++) {
                if (i > 0) {
                    sb.append(", ");
                }
                sb.append('"').append(escapeTomlString(access.privatePaths.get(i))).append('"');
            }
            sb.append("]\n");
        }
        return sb.toString();
    }

    private static String buildPeopleBody(Map<UUID, Integer> people) {
        if (people == null || people.isEmpty()) {
            return "";
        }
        StringBuilder sb = new StringBuilder();
        for (Map.Entry<UUID, Integer> entry : people.entrySet()) {
            sb.append('"').append(entry.getKey()).append("\" = ").append(entry.getValue()).append('\n');
        }
        return sb.toString();
    }

    private static String buildPermNodesBody(Map<String, Integer> nodes) {
        if (nodes == null || nodes.isEmpty()) {
            return "";
        }
        StringBuilder sb = new StringBuilder();
        for (Map.Entry<String, Integer> entry : nodes.entrySet()) {
            sb.append('"').append(escapeTomlString(entry.getKey())).append("\" = ")
                    .append(entry.getValue()).append('\n');
        }
        return sb.toString();
    }

    private static String buildLevelsBody(AccessControlConfig access) {
        StringBuilder sb = new StringBuilder();
        for (int i = 0; i <= 4; i++) {
            sb.append('"').append(i).append("\" = [");
            List<String> caps = new ArrayList<>(access.capabilitiesFor(i));
            for (int c = 0; c < caps.size(); c++) {
                if (c > 0) {
                    sb.append(", ");
                }
                sb.append('"').append(escapeTomlString(caps.get(c))).append('"');
            }
            sb.append("]\n");
        }
        return sb.toString();
    }

    @SuppressWarnings("unchecked")
    private static Map<Integer, Set<String>> parseLevels(Object raw, Map<Integer, Set<String>> fallback) {
        if (raw == null) {
            return fallback;
        }
        if (!(raw instanceof Map<?, ?> map)) {
            throw new IllegalArgumentException("levels must be an object keyed by 0..4");
        }
        Map<Integer, Set<String>> out = new LinkedHashMap<>();
        for (int i = 0; i <= 4; i++) {
            out.put(i, new LinkedHashSet<>(fallback.getOrDefault(i, Collections.emptySet())));
        }
        for (Map.Entry<?, ?> entry : map.entrySet()) {
            int level = clampLevel(intValue(entry.getKey(), -1));
            Object value = entry.getValue();
            if (!(value instanceof List<?> list)) {
                throw new IllegalArgumentException("levels." + level + " must be an array of capabilities");
            }
            Set<String> caps = new LinkedHashSet<>();
            for (Object item : list) {
                caps.add(AccessCapability.requireValid(String.valueOf(item)));
            }
            out.put(level, caps);
        }
        return out;
    }

    @SuppressWarnings("unchecked")
    private static Map<UUID, Integer> parsePeople(Object raw, Map<UUID, Integer> fallback) {
        if (raw == null) {
            return fallback;
        }
        Map<UUID, Integer> out = new LinkedHashMap<>();
        if (raw instanceof List<?> list) {
            for (Object item : list) {
                if (!(item instanceof Map<?, ?> row)) {
                    throw new IllegalArgumentException("people entries must be objects with uuid/level");
                }
                Object uuidRaw = row.get("uuid");
                if (uuidRaw == null) {
                    throw new IllegalArgumentException("people entry missing uuid");
                }
                UUID uuid = parseFlexibleUuid(String.valueOf(uuidRaw));
                int level = clampLevel(intValue(row.get("level"), 4));
                out.put(uuid, level);
            }
            return out;
        }
        if (raw instanceof Map<?, ?> map) {
            for (Map.Entry<?, ?> entry : map.entrySet()) {
                UUID uuid = parseFlexibleUuid(String.valueOf(entry.getKey()));
                out.put(uuid, clampLevel(intValue(entry.getValue(), 4)));
            }
            return out;
        }
        throw new IllegalArgumentException("people must be an array or object");
    }

    private static Map<String, Integer> parsePermNodes(Object raw, Map<String, Integer> fallback) {
        if (raw == null) {
            return fallback;
        }
        if (!(raw instanceof Map<?, ?> map)) {
            throw new IllegalArgumentException("perm_nodes must be an object");
        }
        Map<String, Integer> out = new LinkedHashMap<>();
        for (Map.Entry<?, ?> entry : map.entrySet()) {
            String node = String.valueOf(entry.getKey()).trim();
            if (node.isEmpty()) {
                continue;
            }
            out.put(node, clampLevel(intValue(entry.getValue(), 4)));
        }
        return out;
    }

    private static List<String> parseStringList(Object raw) {
        if (raw == null) {
            return Collections.emptyList();
        }
        if (!(raw instanceof List<?> list)) {
            throw new IllegalArgumentException("private_paths must be an array");
        }
        List<String> out = new ArrayList<>();
        for (Object item : list) {
            String value = String.valueOf(item).trim();
            if (!value.isEmpty()) {
                out.add(value);
            }
        }
        return out;
    }

    /**
     * Accepts a player UUID in either canonical dashed form
     * ({@code 8-4-4-4-12}) or compact 32-hex-digit form (no dashes), matching
     * how UUIDs appear in Mojang APIs and how players often paste them.
     */
    public static UUID parseFlexibleUuid(String raw) {
        if (raw == null) {
            throw new IllegalArgumentException("uuid is required");
        }
        String trimmed = raw.trim();
        if (trimmed.isEmpty()) {
            throw new IllegalArgumentException("uuid is required");
        }
        String compact = trimmed.replace("-", "");
        if (compact.length() != 32 || !compact.matches("[0-9a-fA-F]{32}")) {
            throw new IllegalArgumentException("Invalid UUID: " + trimmed);
        }
        String dashed = compact.substring(0, 8) + "-"
                + compact.substring(8, 12) + "-"
                + compact.substring(12, 16) + "-"
                + compact.substring(16, 20) + "-"
                + compact.substring(20, 32);
        return UUID.fromString(dashed);
    }

    private static int clampLevel(int level) {
        if (level < 0 || level > 4) {
            throw new IllegalArgumentException("level must be between 0 and 4");
        }
        return level;
    }

    private static String stringValue(Object value) {
        return value == null ? null : String.valueOf(value);
    }

    private static int intValue(Object value, int fallback) {
        if (value == null) {
            return fallback;
        }
        if (value instanceof Number number) {
            return number.intValue();
        }
        return Integer.parseInt(String.valueOf(value).trim());
    }

    private static long longValue(Object value, long fallback) {
        if (value == null) {
            return fallback;
        }
        if (value instanceof Number number) {
            return number.longValue();
        }
        return Long.parseLong(String.valueOf(value).trim());
    }

    private static String escapeTomlString(String value) {
        return value
                .replace("\\", "\\\\")
                .replace("\"", "\\\"")
                .replace("\n", "\\n")
                .replace("\r", "\\r")
                .replace("\t", "\\t");
    }

    private static String unescapeTomlString(String value) {
        StringBuilder out = new StringBuilder();
        for (int i = 0; i < value.length(); i++) {
            char c = value.charAt(i);
            if (c == '\\' && i + 1 < value.length()) {
                char n = value.charAt(++i);
                out.append(switch (n) {
                    case 'n' -> '\n';
                    case 'r' -> '\r';
                    case 't' -> '\t';
                    case '"' -> '"';
                    case '\\' -> '\\';
                    default -> n;
                });
            } else {
                out.append(c);
            }
        }
        return out.toString();
    }
}
