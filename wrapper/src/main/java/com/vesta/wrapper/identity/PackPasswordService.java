package com.vesta.wrapper.identity;

import com.vesta.wrapper.audit.AuditActor;
import com.vesta.wrapper.audit.AuditLog;
import com.vesta.wrapper.audit.AuditRecord;
import com.vesta.wrapper.authorization.AuthorizationService;
import com.vesta.wrapper.authorization.Capability;
import com.vesta.wrapper.authorization.Decision;
import com.vesta.wrapper.authorization.EffectivePolicy;
import com.vesta.wrapper.identity.IdentityRecords.RemoteActor;
import com.vesta.wrapper.security.PasswordKdfGate;
import com.vesta.wrapper.state.StateDatabase;
import com.vesta.wrapper.state.StateException;

import java.sql.Connection;
import java.sql.SQLException;
import java.time.Instant;
import java.util.Locale;
import java.util.Map;
import java.util.Objects;
import java.util.Set;
import java.util.function.Supplier;

/**
 * Trusted in-process pack-password storage, not a login endpoint or an authentication adapter.
 * Management requires current canonical credentials.manage authority; actor IDs are not possession proof.
 * A password match is only a server/revision observation, never a principal, device grant or management
 * capability. Results (including their public record constructors) are not proof of password possession;
 * never accept a caller-supplied result/revision as authentication. A future grant issuer must perform its
 * own trusted verification and bind the still-current revision atomically when issuing a device-bound grant.
 * Microsoft/Minecraft identity and its canonical policy remain a separate, primary online-mode adapter.
 *
 * <p>Derivation never holds a database transaction. Production instances share bounded KDF admission.
 * The caller owns the char[] and must clear it; the hasher clears its own encoding/derived buffers.
 * No PHC is exposed in returned records, errors, audit, events or operation inputs.
 */
public final class PackPasswordService {
    public enum VerificationStatus {
        MATCH, MISMATCH, UNAVAILABLE, CHANGED, INVALID_PASSWORD, INVALID_VERIFIER, KDF_UNAVAILABLE,
        RATE_LIMITED, BUSY
    }

    public record Verification(String serverId, long revision, VerificationStatus status) {
        public boolean matched() {
            return status == VerificationStatus.MATCH;
        }
    }

    /** Revision 0 denotes absence; revoked rows retain a positive revision and their original creation time. */
    public record Metadata(String serverId, long revision, boolean active, Instant createdAt,
                           Instant updatedAt, Instant revokedAt) {
    }

    /** Private transient storage record; never let an accidental toString reveal the verifier. */
    private record Snapshot(long revision, String phc, long createdAt, long updatedAt, Long revokedAt) {
        boolean active() {
            return revokedAt == null;
        }

        @Override
        public String toString() {
            return "PackPasswordSnapshot[revision=" + revision + ", active=" + active() + ", phc=<redacted>]";
        }
    }

    /** Trusted package-private fault/concurrency seam, not a network-selectable password algorithm. */
    interface PasswordWork {
        String hash(char[] password);
        boolean verify(char[] password, String phc);
    }

    private static final String TARGET = "pack-access";
    private static final String SOURCE = "pack-password-in-process";
    private static final Set<String> REFUSALS = Set.of("forbidden", "invalid_request", "credential_conflict",
            "credential_revision_exhausted", "invalid_password", "password_kdf_unavailable");
    private final StateDatabase db;
    private final AuditLog audit;
    private final AuthorizationService authorization;
    private final PasswordWork passwords;
    private final PasswordKdfGate gate;

    /** Production defaults, including shared JVM-local admission (two KDFs, 60 verification/10 management per minute). */
    public PackPasswordService(StateDatabase db, AuditLog audit, AuthorizationService authorization) {
        this(db, audit, authorization, passwordWork(new PasswordHasher()), PasswordKdfGate.shared());
    }

    PackPasswordService(StateDatabase db, AuditLog audit, AuthorizationService authorization,
                        PasswordWork passwords, PasswordKdfGate gate) {
        this.db = Objects.requireNonNull(db, "db");
        this.audit = Objects.requireNonNull(audit, "audit");
        this.authorization = Objects.requireNonNull(authorization, "authorization");
        this.passwords = Objects.requireNonNull(passwords, "passwords");
        this.gate = Objects.requireNonNull(gate, "gate");
    }

    static PasswordWork passwordWork(PasswordHasher hasher) {
        Objects.requireNonNull(hasher, "hasher");
        return new PasswordWork() {
            @Override public String hash(char[] password) { return hasher.hash(password); }
            @Override public boolean verify(char[] password, String phc) { return hasher.verify(password, phc); }
        };
    }

    /** Administrative metadata only; no verifier accessor exists. */
    public Metadata metadata(RemoteActor actor) {
        return administrative(actor, "pack_password.inspect", () -> db.read(c -> {
            requirePolicy(c, actor);
            return metadata(load(c));
        }));
    }

    /** Create with expectedRevision=0; rotate/reactivate only at the exact current positive revision. */
    public Metadata setPassword(RemoteActor actor, long expectedRevision, char[] password) {
        return administrative(actor, "pack_password.set", () -> {
            precheck(actor, expectedRevision, true);
            var admission = gate.tryAcquire(PasswordKdfGate.Purpose.MANAGEMENT);
            requireAdmission(admission.status());
            try (var permit = admission.permit()) {
                String phc;
                try {
                    phc = passwords.hash(password);
                } catch (PasswordHashException invalid) {
                    throw failure("invalid_password");
                } catch (RuntimeException unavailable) {
                    throw failure("password_kdf_unavailable");
                }
                return db.write(c -> {
                    EffectivePolicy policy = requirePolicy(c, actor);
                    Snapshot current = load(c);
                    requireRevision(current, expectedRevision, true);
                    long revision = nextRevision(expectedRevision);
                    long now = db.clock().millis();
                    if (current == null) {
                        try (var ps = c.prepareStatement("INSERT INTO pack_passwords "
                                + "(server_id,revision,phc,created_at,updated_at) VALUES (?,?,?,?,?)")) {
                            ps.setString(1, db.serverId()); ps.setLong(2, revision); ps.setString(3, phc);
                            ps.setLong(4, now); ps.setLong(5, now); ps.executeUpdate();
                        }
                    } else {
                        try (var ps = c.prepareStatement("UPDATE pack_passwords SET revision=?,phc=?,updated_at=?,revoked_at=NULL "
                                + "WHERE server_id=? AND revision=?")) {
                            ps.setLong(1, revision); ps.setString(2, phc); ps.setLong(3, now);
                            ps.setString(4, db.serverId()); ps.setLong(5, expectedRevision);
                            if (ps.executeUpdate() != 1) { throw failure("credential_conflict"); }
                        }
                    }
                    audit.append(c, new AuditRecord(actor(policy), "pack_password.set", TARGET, "success", null,
                            Map.of("revision", Long.toString(revision), "previous_revision", Long.toString(expectedRevision))));
                    return metadata(load(c));
                });
            }
        });
    }

    /** Revoke without deleting the singleton or retaining its PHC; even repeated revocation requires a fresh revision. */
    public Metadata revoke(RemoteActor actor, long expectedRevision) {
        return administrative(actor, "pack_password.revoke", () -> {
            precheck(actor, expectedRevision, false);
            // Revocation has no KDF and must remain possible while all derivation slots are busy.
            if (!gate.tryConsumeAttempt(PasswordKdfGate.Purpose.MANAGEMENT)) {
                throw failure("password_rate_limited");
            }
            return db.write(c -> {
                EffectivePolicy policy = requirePolicy(c, actor);
                Snapshot current = load(c);
                requireRevision(current, expectedRevision, false);
                long revision = nextRevision(expectedRevision);
                long now = db.clock().millis();
                try (var ps = c.prepareStatement("UPDATE pack_passwords SET revision=?,phc=NULL,updated_at=?,revoked_at=? "
                        + "WHERE server_id=? AND revision=?")) {
                    ps.setLong(1, revision); ps.setLong(2, now); ps.setLong(3, now);
                    ps.setString(4, db.serverId()); ps.setLong(5, expectedRevision);
                    if (ps.executeUpdate() != 1) { throw failure("credential_conflict"); }
                }
                audit.append(c, new AuditRecord(actor(policy), "pack_password.revoke", TARGET, "success", null,
                        Map.of("revision", Long.toString(revision), "previous_revision", Long.toString(expectedRevision))));
                return metadata(load(c));
            });
        });
    }

    /**
     * No actor/grant is created. Admitted outcomes have a fixed system audit identity because there is no
     * transport here. Rate/busy refusals do not write unbounded audit traffic; a future transport must
     * supply its own bounded source/attempt accounting. No match is returned if final audit fails.
     */
    public Verification verify(char[] password) {
        var admission = gate.tryAcquire(PasswordKdfGate.Purpose.VERIFICATION);
        if (admission.status() != PasswordKdfGate.Status.ADMITTED) {
            return new Verification(db.serverId(), 0, admission.status() == PasswordKdfGate.Status.BUSY
                    ? VerificationStatus.BUSY : VerificationStatus.RATE_LIMITED);
        }
        try (var permit = admission.permit()) {
            Snapshot snapshot = db.read(this::load);
            VerificationStatus computed = VerificationStatus.UNAVAILABLE;
            if (snapshot != null && snapshot.active()) {
                try {
                    computed = passwords.verify(password, snapshot.phc())
                            ? VerificationStatus.MATCH : VerificationStatus.MISMATCH;
                } catch (PasswordHashException invalid) {
                    computed = PasswordHashException.INVALID_PASSWORD.equals(invalid.getCode())
                            ? VerificationStatus.INVALID_PASSWORD : VerificationStatus.INVALID_VERIFIER;
                } catch (RuntimeException unavailable) {
                    computed = VerificationStatus.KDF_UNAVAILABLE;
                }
            }
            return finishVerification(snapshot, computed);
        } catch (StateException databaseFailure) {
            // SQL engines/triggers can include parameter values in their causes; never export that chain.
            throw failure(databaseFailure.getCode());
        }
    }

    private Verification finishVerification(Snapshot snapshot, VerificationStatus computed) {
        return db.write(c -> {
            Snapshot current = load(c);
            VerificationStatus status = sameRevision(snapshot, current) ? computed : VerificationStatus.CHANGED;
            long revision = snapshot == null ? 0 : snapshot.revision();
            audit.append(c, new AuditRecord(AuditActor.system(SOURCE), "pack_password.verify", TARGET,
                    status.name().toLowerCase(Locale.ROOT), null, Map.of("revision", Long.toString(revision))));
            return new Verification(db.serverId(), revision, status);
        });
    }

    private static boolean sameRevision(Snapshot before, Snapshot after) {
        if (before == null || after == null) { return before == after; }
        // Comparing the bounded PHC too fails closed on accidental same-revision storage alteration.
        return before.revision() == after.revision() && Objects.equals(before.revokedAt(), after.revokedAt())
                && Objects.equals(before.phc(), after.phc());
    }

    private void precheck(RemoteActor actor, long expectedRevision, boolean allowCreation) {
        db.read(c -> {
            requirePolicy(c, actor);
            requireRevision(load(c), expectedRevision, allowCreation);
            nextRevision(expectedRevision);
            return null;
        });
    }

    private EffectivePolicy requirePolicy(Connection c, RemoteActor actor) throws SQLException {
        if (actor == null) { throw failure("forbidden"); }
        EffectivePolicy policy = authorization.resolve(c, actor.principalId(), actor.deviceId());
        Decision decision = policy.check(Capability.CREDENTIALS_MANAGE, "");
        if (!decision.allowed()) { throw failure("forbidden"); }
        return policy;
    }

    private static void requireRevision(Snapshot current, long expected, boolean allowCreation) {
        if (expected < 0 || (!allowCreation && expected == 0)) { throw failure("invalid_request"); }
        if ((current == null && expected != 0) || (current != null && current.revision() != expected)) {
            throw failure("credential_conflict");
        }
    }

    private static long nextRevision(long current) {
        if (current == Long.MAX_VALUE) { throw failure("credential_revision_exhausted"); }
        return current + 1;
    }

    private Snapshot load(Connection c) throws SQLException {
        try (var ps = c.prepareStatement("SELECT revision,phc,created_at,updated_at,revoked_at FROM pack_passwords WHERE server_id=?")) {
            ps.setString(1, db.serverId());
            try (var row = ps.executeQuery()) {
                if (!row.next()) { return null; }
                long revoked = row.getLong(5);
                Long revokedAt = row.wasNull() ? null : revoked;
                Snapshot snapshot = new Snapshot(row.getLong(1), row.getString(2), row.getLong(3), row.getLong(4), revokedAt);
                if (snapshot.revision() < 1 || (snapshot.active() != (snapshot.phc() != null))) {
                    throw failure("credential_state_corrupt");
                }
                return snapshot;
            }
        }
    }

    private Metadata metadata(Snapshot snapshot) {
        return snapshot == null ? new Metadata(db.serverId(), 0, false, null, null, null)
                : new Metadata(db.serverId(), snapshot.revision(), snapshot.active(), Instant.ofEpochMilli(snapshot.createdAt()),
                Instant.ofEpochMilli(snapshot.updatedAt()), snapshot.revokedAt() == null ? null : Instant.ofEpochMilli(snapshot.revokedAt()));
    }

    private <T> T administrative(RemoteActor actor, String action, Supplier<T> work) {
        try {
            return work.get();
        } catch (StateException refused) {
            if (REFUSALS.contains(refused.getCode())) {
                try {
                    audit.append(new AuditRecord(safeActor(actor), action, TARGET, "denied", null,
                            Map.of("reason", refused.getCode())));
                } catch (RuntimeException auditFailure) {
                    // Fixed error only: no crypto/SQL engine exception text is exported by this refusal path.
                    throw failure("audit_unavailable");
                }
            }
            // Keep the stable code but not a potentially secret-bearing database/audit cause or message.
            throw failure(refused.getCode());
        }
    }

    private static AuditActor actor(EffectivePolicy policy) {
        return new AuditActor(policy.principalId(), policy.deviceId(), SOURCE);
    }

    private static AuditActor safeActor(RemoteActor actor) {
        try {
            return actor == null ? new AuditActor("unknown", "unknown", SOURCE)
                    : new AuditActor(actor.principalId(), actor.deviceId(), SOURCE);
        } catch (RuntimeException malformed) {
            return new AuditActor("unknown", "unknown", SOURCE);
        }
    }

    private static void requireAdmission(PasswordKdfGate.Status status) {
        if (status == PasswordKdfGate.Status.BUSY) { throw failure("password_kdf_busy"); }
        if (status == PasswordKdfGate.Status.RATE_LIMITED) { throw failure("password_rate_limited"); }
    }

    private static StateException failure(String code) {
        return new StateException(code, "Pack password request failed: " + code);
    }
}
