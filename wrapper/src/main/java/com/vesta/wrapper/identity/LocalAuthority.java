package com.vesta.wrapper.identity;

import java.time.Instant;
import java.util.Optional;

/**
 * Trusted, OS-authorized local authority (e.g. a future protected named-pipe/Unix-socket CLI channel
 * whose peer identity and ACL the OS has verified). It is the ONLY way to perform owner bootstrap,
 * owner recovery or local Owner/Administrator grants.
 *
 * <p>Composition passes a concrete implementation; there is deliberately no implementation in this
 * package other than {@link #UNAVAILABLE}. A caller-supplied boolean, a loopback address, an OP flag,
 * a connector message or any network input is never a proof. {@link IdentityService} additionally
 * enforces purpose binding, freshness, a bounded lifetime and single use of each nonce.
 */
public interface LocalAuthority {

    /** What a proof authorizes. A proof is only accepted for exactly its attested purpose. */
    enum Purpose {
        OWNER_BOOTSTRAP("owner_bootstrap"),
        OWNER_RECOVERY("owner_recovery"),
        LOCAL_ROLE_GRANT("local_role_grant");

        private final String id;

        Purpose(String id) {
            this.id = id;
        }

        public String id() {
            return id;
        }
    }

    /** Opaque proof object minted by the OS-authorized channel; only its issuing authority can verify it. */
    interface Proof {
    }

    /**
     * What the authority vouches for.
     *
     * @param nonce     unique per proof (high entropy); recorded hashed so the proof is single-use
     * @param purpose   the single purpose this proof authorizes
     * @param osActor   non-secret description of the verified OS identity, for audit (e.g. "DOMAIN\\user")
     * @param issuedAt  when the OS check happened
     * @param expiresAt end of validity; lifetime is capped by {@link IdentityService#MAX_PROOF_LIFETIME}
     */
    record Attestation(String nonce, Purpose purpose, String osActor, Instant issuedAt, Instant expiresAt) {
        @Override
        public String toString() {
            return "Attestation[purpose=" + purpose + ", osActor=" + osActor + ", issuedAt=" + issuedAt
                    + ", expiresAt=" + expiresAt + ", nonce=<redacted>]";
        }
    }

    /**
     * Verifies that {@code proof} was minted by this authority for {@code expectedPurpose}.
     * Must return empty for foreign, malformed or mismatched proofs.
     */
    Optional<Attestation> verify(Proof proof, Purpose expectedPurpose);

    /** Default: no local authority is wired, so every local operation is refused. */
    LocalAuthority UNAVAILABLE = (proof, purpose) -> Optional.empty();
}
