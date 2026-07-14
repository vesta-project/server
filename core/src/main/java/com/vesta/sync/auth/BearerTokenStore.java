package com.vesta.sync.auth;

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
            return Instant.now().isAfter(expiresAt);
        }
    }

    private final ConcurrentHashMap<String, Record> byHash = new ConcurrentHashMap<>();
    private final long ttlSeconds;

    public BearerTokenStore(long ttlSeconds) {
        this.ttlSeconds = ttlSeconds;
    }

    public String issue(String role, UUID boundUuid) {
        String raw = TokenService.issueOpaqueToken();
        String hash = TokenService.hashToken(raw);
        Instant now = Instant.now();
        byHash.put(hash, new Record(hash, role, boundUuid, now, now.plusSeconds(ttlSeconds)));
        return raw;
    }

    public Optional<Record> validate(String rawToken) {
        if (rawToken == null || rawToken.trim().isEmpty()) {
            return Optional.empty();
        }
        String hash = TokenService.hashToken(rawToken);
        Record record = byHash.get(hash);
        if (record == null || record.isExpired()) {
            if (record != null) {
                byHash.remove(hash, record);
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
        byHash.entrySet().removeIf(e -> e.getValue().isExpired());
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
