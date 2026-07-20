package com.vesta.sync.auth;

import java.nio.charset.StandardCharsets;
import java.security.MessageDigest;
import java.security.SecureRandom;
import java.time.Clock;
import java.time.Duration;
import java.time.Instant;
import java.util.Optional;
import java.util.UUID;
import java.util.concurrent.ConcurrentHashMap;

/**
 * Issues one-time server hashes for Mojang's normal session join service.
 * Minecraft access tokens never pass through Vesta.
 */
public final class MinecraftSessionChallengeService {

    public static final class VerifiedProfile {
        private final UUID uuid;
        private final String name;

        public VerifiedProfile(UUID uuid, String name) {
            this.uuid = uuid;
            this.name = name;
        }

        public UUID uuid() { return uuid; }
        public String name() { return name; }
    }

    public static final class IssuedChallenge {
        private final String challengeId;
        private final String serverId;
        private final Instant expiresAt;

        public IssuedChallenge(String challengeId, String serverId, Instant expiresAt) {
            this.challengeId = challengeId;
            this.serverId = serverId;
            this.expiresAt = expiresAt;
        }

        public String challengeId() { return challengeId; }
        public String serverId() { return serverId; }
        public Instant expiresAt() { return expiresAt; }
    }

    @FunctionalInterface
    public interface SessionVerifier {
        Optional<VerifiedProfile> verify(String username, String serverId);
    }

    private static final class Pending {
        private final UUID serverUuid;
        private final UUID profileUuid;
        private final String profileName;
        private final String serverId;
        private final Instant expiresAt;

        private Pending(UUID serverUuid, UUID profileUuid, String profileName,
                        String serverId, Instant expiresAt) {
            this.serverUuid = serverUuid;
            this.profileUuid = profileUuid;
            this.profileName = profileName;
            this.serverId = serverId;
            this.expiresAt = expiresAt;
        }
    }

    private final ConcurrentHashMap<String, Pending> pending = new ConcurrentHashMap<>();
    private final Duration ttl;
    private final SessionVerifier verifier;
    private final SecureRandom random;
    private final Clock clock;

    public MinecraftSessionChallengeService(Duration ttl, SessionVerifier verifier) {
        this(ttl, verifier, new SecureRandom(), Clock.systemUTC());
    }

    MinecraftSessionChallengeService(
            Duration ttl,
            SessionVerifier verifier,
            SecureRandom random,
            Clock clock
    ) {
        this.ttl = ttl;
        this.verifier = verifier;
        this.random = random;
        this.clock = clock;
    }

    public IssuedChallenge issue(UUID serverUuid, UUID profileUuid, String profileName) {
        if (serverUuid == null || profileUuid == null) {
            throw new IllegalArgumentException("server and profile UUIDs are required");
        }
        String normalizedName = profileName == null ? "" : profileName.trim();
        if (!normalizedName.matches("[A-Za-z0-9_]{1,16}")) {
            throw new IllegalArgumentException("invalid minecraft profile name");
        }

        byte[] nonce = new byte[32];
        random.nextBytes(nonce);
        String challengeId = toHex(nonce);
        String serverId = digest(serverUuid + ":" + challengeId);
        Instant expiresAt = clock.instant().plus(ttl);
        pending.put(
                challengeId,
                new Pending(serverUuid, profileUuid, normalizedName, serverId, expiresAt)
        );
        purgeExpired();
        return new IssuedChallenge(challengeId, serverId, expiresAt);
    }

    /** Every completion attempt consumes the challenge before remote verification. */
    public Optional<VerifiedProfile> complete(UUID serverUuid, String challengeId) {
        if (serverUuid == null || challengeId == null || challengeId.trim().isEmpty()) {
            return Optional.empty();
        }
        Pending item = pending.remove(challengeId.trim());
        if (item == null
                || !item.serverUuid.equals(serverUuid)
                || !clock.instant().isBefore(item.expiresAt)) {
            return Optional.empty();
        }
        Optional<VerifiedProfile> verified = verifier.verify(item.profileName, item.serverId);
        if (!verified.isPresent()) {
            return Optional.empty();
        }
        VerifiedProfile profile = verified.get();
        if (!item.profileUuid.equals(profile.uuid())
                || !item.profileName.equalsIgnoreCase(profile.name())) {
            return Optional.empty();
        }
        return verified;
    }

    public void purgeExpired() {
        Instant now = clock.instant();
        pending.entrySet().removeIf(entry -> !now.isBefore(entry.getValue().expiresAt));
    }

    public int size() {
        purgeExpired();
        return pending.size();
    }

    /**
     * Derives the one-time Mojang session {@code serverId}. Mojang's
     * {@code joinServer}/{@code hasJoined} endpoints only accept a Minecraft
     * server hash of at most 40 hex characters (SHA-1 sized); a longer digest
     * (e.g. SHA-256) is rejected with {@code "Invalid serverId"}. The value only
     * needs to be unpredictable and identical on both the join and hasJoined
     * calls, so a SHA-1 hex digest is sufficient and Mojang-compatible.
     */
    private static String digest(String input) {
        try {
            MessageDigest md = MessageDigest.getInstance("SHA-1");
            return toHex(md.digest(input.getBytes(StandardCharsets.UTF_8)));
        } catch (Exception e) {
            throw new IllegalStateException("SHA-1 unavailable", e);
        }
    }

    private static String toHex(byte[] bytes) {
        StringBuilder result = new StringBuilder(bytes.length * 2);
        for (byte value : bytes) {
            result.append(String.format("%02x", value & 0xff));
        }
        return result.toString();
    }
}
