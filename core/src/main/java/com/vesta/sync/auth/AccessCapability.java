package com.vesta.sync.auth;

import java.util.Arrays;
import java.util.Collections;
import java.util.HashSet;
import java.util.Locale;
import java.util.Set;

/**
 * Named capabilities granted by access levels.
 *
 * <p>Valid values:
 * <ul>
 *   <li>{@code stats.read} — public descriptor / basic status</li>
 *   <li>{@code pack.fetch} — download/sync the modpack (known + private pack files)</li>
 *   <li>{@code console.read} — view live console/logs</li>
 *   <li>{@code files.read} — browse server files read-only</li>
 *   <li>{@code console.execute} — run console commands</li>
 *   <li>{@code files.write} — modify server files</li>
 *   <li>{@code settings.manage} — change server settings</li>
 *   <li>{@code content.manage} — add/remove mods/plugins on the server</li>
 * </ul>
 */
public final class AccessCapability {

    public static final String STATS_READ = "stats.read";
    public static final String PACK_FETCH = "pack.fetch";
    public static final String CONSOLE_READ = "console.read";
    public static final String FILES_READ = "files.read";
    public static final String CONSOLE_EXECUTE = "console.execute";
    public static final String FILES_WRITE = "files.write";
    public static final String SETTINGS_MANAGE = "settings.manage";
    public static final String CONTENT_MANAGE = "content.manage";

    private static final Set<String> ALL = Collections.unmodifiableSet(new HashSet<String>(Arrays.asList(
            STATS_READ,
            PACK_FETCH,
            CONSOLE_READ,
            FILES_READ,
            CONSOLE_EXECUTE,
            FILES_WRITE,
            SETTINGS_MANAGE,
            CONTENT_MANAGE
    )));

    private AccessCapability() {}

    public static Set<String> all() {
        return ALL;
    }

    public static String requireValid(String raw) {
        if (raw == null || raw.trim().isEmpty()) {
            throw new IllegalArgumentException("Capability name must not be empty");
        }
        String value = raw.trim().toLowerCase(Locale.ROOT);
        if (!ALL.contains(value)) {
            throw new IllegalArgumentException(
                    "Unknown capability '" + raw + "'. Valid values: " + ALL
            );
        }
        return value;
    }
}
