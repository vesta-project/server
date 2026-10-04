package com.vesta.wrapper.identity;

import java.time.Instant;

/** Value types returned by {@link IdentityService}. */
public final class IdentityRecords {
    private IdentityRecords() {
    }

    public record Principal(String id, String displayName, String status, Instant createdAt) {
    }

    public record Device(String id, String principalId, String label, String publicKeySha256,
                         Instant createdAt, Instant revokedAt) {
        public boolean revoked() {
            return revokedAt != null;
        }
    }

    /**
     * A freshly issued single-use bootstrap/recovery code. The plaintext exists only in this object
     * (only its SHA-256 is persisted); {@link #toString()} redacts it so it cannot reach logs by accident.
     */
    public record BootstrapCode(String code, LocalAuthority.Purpose purpose, Instant expiresAt) {
        @Override
        public String toString() {
            return "BootstrapCode[purpose=" + purpose + ", expiresAt=" + expiresAt + ", code=<redacted>]";
        }
    }

    /** Result of redeeming an owner bootstrap/recovery code. */
    public record PairedOwner(String principalId, String deviceId) {
    }

    /**
     * Identifiers of an already-authenticated remote caller. These are lookup keys only and confer no
     * authority by themselves: they MUST come from the authenticated device transport (future mTLS
     * composition), never from request bodies, headers or loopback location. Every remote mutation
     * re-resolves current policy from storage inside its transaction.
     */
    public record RemoteActor(String principalId, String deviceId) {
    }
}
