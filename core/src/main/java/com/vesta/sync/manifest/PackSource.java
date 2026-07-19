package com.vesta.sync.manifest;

/**
 * How the wrapper obtains the client pack.
 *
 * <ul>
 *   <li>{@code modrinth} / {@code curseforge} — reference an existing platform pack</li>
 *   <li>{@code archive} — serve a local {@code .zip}/{@code .mrpack} as one blob</li>
 *   <li>{@code self-hosted} — scan live server files and publish a Vesta pack</li>
 * </ul>
 */
public enum PackSource {
    MODRINTH,
    CURSEFORGE,
    ARCHIVE,
    SELF_HOSTED;

    public static PackSource parse(String raw) {
        if (raw == null || raw.trim().isEmpty()) {
            return SELF_HOSTED;
        }
        String value = raw.trim().toLowerCase();
        if ("modrinth".equals(value)) {
            return MODRINTH;
        }
        if ("curseforge".equals(value)) {
            return CURSEFORGE;
        }
        if ("archive".equals(value)) {
            return ARCHIVE;
        }
        if ("self-hosted".equals(value)
                || "selfhosted".equals(value)
                || "custom".equals(value)
                || "scan".equals(value)) {
            return SELF_HOSTED;
        }
        throw new IllegalArgumentException(
                "Unknown modpack.source '" + raw + "'. "
                        + "Valid values: modrinth, curseforge, archive, self-hosted"
        );
    }

    public boolean isPlatform() {
        return this == MODRINTH || this == CURSEFORGE;
    }

    public String wireName() {
        switch (this) {
            case MODRINTH:
                return "modrinth";
            case CURSEFORGE:
                return "curseforge";
            case ARCHIVE:
                return "archive";
            case SELF_HOSTED:
            default:
                return "self-hosted";
        }
    }
}
