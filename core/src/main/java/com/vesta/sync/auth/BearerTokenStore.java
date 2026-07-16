package com.vesta.sync.auth;

import java.time.Clock;
import java.time.Instant;
import java.util.Objects;
import java.util.Optional;
import java.util.UUID;
import java.util.concurrent.ConcurrentHashMap;

/**
 * In-memory store of issued bearer tokens keyed by SHA-256 of the raw token.
 */
public final class BearerTokenStore {

    public static final class Record {
        public final String tokenHash;
        public final String role;
        public final UUID boundUuid;
        public final Instant issuedAt;
        public final Instant expiresAt;

        public Record(String tokenHash, String role, UUID boundUuid, Instant issuedAt, Instant expiresAt) {
            this.tokenHash = tokenHash;
            this.role = role;
            this.boundUuid = boundUuid;
            this.issuedAt = issuedAt;
            this.expiresAt = expiresAt;
        }

        public boolean isExpired() {
            return !Instant.now().isBefore(expiresAt);
        }

        boolean isExpired(Instant now) {
            return !now.isBefore(expiresAt);
        }
    }

    private final ConcurrentHashMap<String, Record> byHash = new ConcurrentHashMap<>();
    private final long ttlSeconds;
    private final Clock clock;

    public BearerTokenStore(long ttlSeconds) {
        this(ttlSeconds, Clock.systemUTC());
    }

    BearerTokenStore(long ttlSeconds, Clock clock) {
        this.ttlSeconds = ttlSeconds;
        this.clock = Objects.requireNonNull(clock, "clock");
    }

    public String issue(String role, UUID boundUuid) {
        String raw = TokenService.issueOpaqueToken();
        String hash = TokenService.hashToken(raw);
        Instant now = clock.instant();
        byHash.put(hash, new Record(hash, role, boundUuid, now, now.plusSeconds(ttlSeconds)));
        return raw;
    }

    public Optional<Record> validate(String rawToken) {
        if (rawToken == null || rawToken.trim().isEmpty()) {
            return Optional.empty();
        }
        String hash = TokenService.hashToken(rawToken);
        Record record = byHash.get(hash);
        if (record == null || record.isExpired(clock.instant())) {
            if (record != null) {
                byHash.remove(hash, record);
            }
            return Optional.empty();
        }
        return Optional.of(record);
    }

    /** Revalidate an already authenticated session without retaining its raw bearer. */
    public Optional<Record> validateHash(String tokenHash) {
        if (tokenHash == null || tokenHash.trim().isEmpty()) {
            return Optional.empty();
        }
        Record record = byHash.get(tokenHash);
        if (record == null || record.isExpired(clock.instant())) {
            if (record != null) {
                byHash.remove(tokenHash, record);
            }
            return Optional.empty();
        }
        return Optional.of(record);
    }

    public void revoke(String rawToken) {
        if (rawToken == null || rawToken.trim().isEmpty()) {
            return;
        }
        byHash.remove(TokenService.hashToken(rawToken));
    }

    public int size() {
        return byHash.size();
    }

    public void purgeExpired() {
        Instant now = clock.instant();
        byHash.entrySet().removeIf(e -> e.getValue().isExpired(now));
    }

    @Override
    public String toString() {
        return "BearerTokenStore{size=" + byHash.size() + ", ttlSeconds=" + ttlSeconds + "}";
    }

    @Override
    public boolean equals(Object o) {
        return this == o;
    }

    @Override
    public int hashCode() {
        return Objects.hashCode(ttlSeconds);
    }
}
