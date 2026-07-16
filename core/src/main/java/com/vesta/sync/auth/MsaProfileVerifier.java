package com.vesta.sync.auth;

import com.google.gson.JsonObject;
import com.google.gson.JsonParser;
import org.slf4j.Logger;
import org.slf4j.LoggerFactory;

import java.io.BufferedReader;
import java.io.InputStreamReader;
import java.net.HttpURLConnection;
import java.net.URL;
import java.net.URLEncoder;
import java.nio.charset.StandardCharsets;
import java.util.Optional;
import java.util.UUID;

/**
 * One-shot Minecraft profile lookup. Token is never stored.
 */
public final class MsaProfileVerifier {

    private static final Logger log = LoggerFactory.getLogger(MsaProfileVerifier.class);
    private static final String PROFILE_URL = "https://api.minecraftservices.com/minecraft/profile";
    private static final String HAS_JOINED_URL =
            "https://sessionserver.mojang.com/session/minecraft/hasJoined";

    public static final class Profile {
        public final UUID uuid;
        public final String name;

        public Profile(UUID uuid, String name) {
            this.uuid = uuid;
            this.name = name;
        }
    }

    public Optional<Profile> verify(String accessToken) {
        if (accessToken == null || accessToken.trim().isEmpty()) {
            return Optional.empty();
        }
        try {
            HttpURLConnection conn = (HttpURLConnection) new URL(PROFILE_URL).openConnection();
            conn.setRequestMethod("GET");
            conn.setRequestProperty("Authorization", "Bearer " + accessToken.trim());
            conn.setConnectTimeout(10_000);
            conn.setReadTimeout(10_000);
            int code = conn.getResponseCode();
            if (code != 200) {
                log.debug("MSA profile returned HTTP {}", code);
                return Optional.empty();
            }
            StringBuilder sb = new StringBuilder();
            try (BufferedReader reader = new BufferedReader(
                    new InputStreamReader(conn.getInputStream(), StandardCharsets.UTF_8))) {
                String line;
                while ((line = reader.readLine()) != null) {
                    sb.append(line);
                }
            }
            JsonObject json = new JsonParser().parse(sb.toString()).getAsJsonObject();
            if (!json.has("id")) {
                return Optional.empty();
            }
            String id = json.get("id").getAsString().replace("-", "");
            UUID uuid = parseUuid(id);
            String name = json.has("name") ? json.get("name").getAsString() : "";
            return Optional.of(new Profile(uuid, name));
        } catch (Exception e) {
            log.warn("MSA profile verification failed: {}", e.getMessage());
            return Optional.empty();
        }
    }

    /**
     * Verifies the result of a launcher-to-Mojang join call. No Minecraft
     * access token is sent to this service or to the Vesta wrapper.
     */
    public Optional<Profile> verifyJoined(String username, String serverId) {
        if (username == null || username.trim().isEmpty()
                || serverId == null || serverId.trim().isEmpty()) {
            return Optional.empty();
        }
        try {
            String query = "?username="
                    + URLEncoder.encode(username.trim(), StandardCharsets.UTF_8.name())
                    + "&serverId="
                    + URLEncoder.encode(serverId.trim(), StandardCharsets.UTF_8.name());
            HttpURLConnection conn = (HttpURLConnection) new URL(HAS_JOINED_URL + query)
                    .openConnection();
            conn.setRequestMethod("GET");
            conn.setConnectTimeout(10_000);
            conn.setReadTimeout(10_000);
            int code = conn.getResponseCode();
            if (code != 200) {
                log.debug("Mojang hasJoined returned HTTP {}", code);
                return Optional.empty();
            }
            StringBuilder sb = new StringBuilder();
            try (BufferedReader reader = new BufferedReader(
                    new InputStreamReader(conn.getInputStream(), StandardCharsets.UTF_8))) {
                String line;
                while ((line = reader.readLine()) != null) {
                    sb.append(line);
                }
            }
            JsonObject json = JsonParser.parseString(sb.toString()).getAsJsonObject();
            if (!json.has("id") || !json.has("name")) {
                return Optional.empty();
            }
            return Optional.of(new Profile(
                    parseUuid(json.get("id").getAsString().replace("-", "")),
                    json.get("name").getAsString()
            ));
        } catch (Exception e) {
            log.warn("Mojang session verification failed: {}", e.getMessage());
            return Optional.empty();
        }
    }

    private static UUID parseUuid(String compact) {
        if (compact.length() != 32) {
            throw new IllegalArgumentException("Invalid UUID length");
        }
        String dashed = compact.substring(0, 8) + "-"
                + compact.substring(8, 12) + "-"
                + compact.substring(12, 16) + "-"
                + compact.substring(16, 20) + "-"
                + compact.substring(20, 32);
        return UUID.fromString(dashed);
    }
}
