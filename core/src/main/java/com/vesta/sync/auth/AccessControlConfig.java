package com.vesta.sync.auth;

import java.util.ArrayList;
import java.util.Collections;
import java.util.HashMap;
import java.util.HashSet;
import java.util.LinkedHashMap;
import java.util.LinkedHashSet;
import java.util.List;
import java.util.Map;
import java.util.Set;
import java.util.UUID;

/**
 * Config-driven access levels (0–4) and the capabilities each level grants.
 *
 * <p>Levels need not be cumulative, but the bundled defaults are. Minecraft OP
 * levels contribute the same numeric Vesta level directly (no custom OP map).
 */
public final class AccessControlConfig {

    public final Map<Integer, Set<String>> levels;
    public final int anonymousBaseline;
    public final int authenticatedBaseline;
    public final int passwordBaseline;
    public final int linkedBaseline;
    public final int whitelistBaseline;
    public final Map<UUID, Integer> people;
    public final Map<String, Integer> permNodes;
    public final List<String> privatePaths;
    /** Compatibility: pack.fetch requires auth when anonymous baseline lacks it. */
    public final boolean accessRequiresAuth;

    public AccessControlConfig(
            Map<Integer, Set<String>> levels,
            int anonymousBaseline,
            int authenticatedBaseline,
            int passwordBaseline,
            int linkedBaseline,
            int whitelistBaseline,
            Map<UUID, Integer> people,
            Map<String, Integer> permNodes,
            List<String> privatePaths
    ) {
        this.levels = freezeLevels(levels);
        this.anonymousBaseline = clamp(anonymousBaseline);
        this.authenticatedBaseline = clamp(authenticatedBaseline);
        this.passwordBaseline = clamp(passwordBaseline);
        this.linkedBaseline = clamp(linkedBaseline);
        this.whitelistBaseline = clamp(whitelistBaseline);
        this.people = Collections.unmodifiableMap(new LinkedHashMap<UUID, Integer>(
                people != null ? people : Collections.<UUID, Integer>emptyMap()
        ));
        this.permNodes = Collections.unmodifiableMap(new LinkedHashMap<String, Integer>(
                permNodes != null ? permNodes : Collections.<String, Integer>emptyMap()
        ));
        this.privatePaths = privatePaths != null
                ? Collections.unmodifiableList(new ArrayList<String>(privatePaths))
                : Collections.<String>emptyList();
        this.accessRequiresAuth = !capabilitiesFor(this.anonymousBaseline).contains(AccessCapability.PACK_FETCH);
    }

    public Set<String> capabilitiesFor(int level) {
        Set<String> caps = levels.get(Integer.valueOf(clamp(level)));
        return caps != null ? caps : Collections.<String>emptySet();
    }

    public static AccessControlConfig defaults() {
        return new AccessControlConfig(
                defaultLevelMap(),
                0,  // anonymous
                1,  // authenticated — pack.fetch
                4,  // password — full operator suite
                1,  // linked — pack.fetch
                3,  // whitelist — files.read
                Collections.<UUID, Integer>emptyMap(),
                Collections.<String, Integer>emptyMap(),
                Collections.<String>emptyList()
        );
    }

    private static int clamp(int level) {
        if (level < 0) {
            return 0;
        }
        if (level > 4) {
            return 4;
        }
        return level;
    }

    private static Map<Integer, Set<String>> freezeLevels(Map<Integer, Set<String>> levels) {
        Map<Integer, Set<String>> fallback = defaultLevelMap();
        Map<Integer, Set<String>> source = levels != null ? levels : fallback;
        Map<Integer, Set<String>> out = new LinkedHashMap<Integer, Set<String>>();
        for (int i = 0; i <= 4; i++) {
            Set<String> caps = source.get(Integer.valueOf(i));
            if (caps == null) {
                caps = fallback.get(Integer.valueOf(i));
            }
            Set<String> normalized = new LinkedHashSet<String>();
            for (String cap : caps) {
                normalized.add(AccessCapability.requireValid(cap));
            }
            out.put(Integer.valueOf(i), Collections.unmodifiableSet(normalized));
        }
        return Collections.unmodifiableMap(out);
    }

    private static Map<Integer, Set<String>> defaultLevelMap() {
        Map<Integer, Set<String>> levels = new HashMap<Integer, Set<String>>();
        levels.put(0, setOf(AccessCapability.STATS_READ));
        levels.put(1, setOf(AccessCapability.STATS_READ, AccessCapability.PACK_FETCH));
        levels.put(2, setOf(
                AccessCapability.STATS_READ,
                AccessCapability.PACK_FETCH,
                AccessCapability.CONSOLE_READ
        ));
        levels.put(3, setOf(
                AccessCapability.STATS_READ,
                AccessCapability.PACK_FETCH,
                AccessCapability.CONSOLE_READ,
                AccessCapability.FILES_READ
        ));
        levels.put(4, setOf(
                AccessCapability.STATS_READ,
                AccessCapability.PACK_FETCH,
                AccessCapability.CONSOLE_READ,
                AccessCapability.FILES_READ,
                AccessCapability.CONSOLE_EXECUTE,
                AccessCapability.FILES_WRITE,
                AccessCapability.SETTINGS_MANAGE,
                AccessCapability.CONTENT_MANAGE
        ));
        return levels;
    }

    private static Set<String> setOf(String... values) {
        Set<String> set = new HashSet<String>();
        Collections.addAll(set, values);
        return set;
    }
}
