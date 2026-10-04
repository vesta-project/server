package com.vesta.wrapper.operations;

import com.vesta.wrapper.audit.AuditActor;
import com.vesta.wrapper.audit.AuditLog;
import com.vesta.wrapper.audit.AuditRecord;
import com.vesta.wrapper.state.StateDatabase;
import com.vesta.wrapper.state.StateValues;

import java.security.SecureRandom;
import java.sql.Connection;
import java.sql.PreparedStatement;
import java.sql.ResultSet;
import java.sql.SQLException;
import java.time.Duration;
import java.time.Instant;
import java.util.Base64;
import java.util.Map;
import java.util.Objects;
import java.util.UUID;

/**
 * Exact, single-use destructive-operation confirmations.
 *
 * <p>A confirmation binds actor principal, device, action (operation type), target, payload hash,
 * source revision and expiry. Only the SHA-256 of the one-time token is stored. Consumption is an
 * atomic conditional update in the same transaction that moves the operation from
 * awaiting_confirmation to queued. Any mismatch, reuse or expiry is rejected, audited, and burns
 * the presented token.
 *
 * <p>The public {@link #issue}/{@link #confirm} methods are <em>trusted in-process</em> APIs: their
 * {@code action}/{@code target}/{@code payloadHash}/{@code sourceRevision} arguments must come from the
 * server's own view of the target state at issue and at confirm time (not from a client), and they do
 * not re-check current authorization policy. Remote/user confirmations must go through
 * {@link PolicyConfirmationService}, which derives all of these server-side and rechecks policy in the
 * same transaction via the package-private connection-taking methods below.
 */
public final class ConfirmationService {
    public static final Duration MAX_TTL = Duration.ofMinutes(15);
    private static final SecureRandom RANDOM = new SecureRandom();
    static final int MAX_TOKEN_CHARS = 128;

    /** Returned once to the requester; {@code token} is never persisted. */
    public record IssuedConfirmation(String confirmationId, String token, String operationId,
                                     String actorPrincipalId, String deviceId, String action, String target,
                                     String payloadHash, String sourceRevision, Instant expiresAt) {
        @Override
        public String toString() {
            return "IssuedConfirmation[" + confirmationId + ", op=" + operationId + ", token=<redacted>]";
        }
    }

    private final StateDatabase db;
    private final AuditLog audit;
    private final OperationService operations;

    public ConfirmationService(StateDatabase db, AuditLog audit, OperationService operations) {
        this.db = db;
        this.audit = audit;
        this.operations = operations;
    }

    StateDatabase database() {
        return db;
    }

    OperationService operations() {
        return operations;
    }

    AuditLog audit() {
        return audit;
    }

    /** Trusted issue; see the class documentation. */
    public IssuedConfirmation issue(AuditActor actor, String operationId, String sourceRevision, Duration ttl) {
        Objects.requireNonNull(actor, "actor");
        StateValues.requireText("sourceRevision", sourceRevision, 128);
        if (!validTtl(ttl)) {
            throw new OperationException(OperationException.INVALID_REQUEST, "confirmation ttl must be within 0..15m");
        }
        return db.write(c -> {
            Operation op = operations.require(c, operationId);
            if (op.state() != OperationState.AWAITING_CONFIRMATION) {
                throw new OperationException(OperationException.INVALID_TRANSITION, "Operation is not awaiting confirmation");
            }
            if (!op.actorPrincipalId().equals(actor.principalId()) || !op.deviceId().equals(actor.deviceId())) {
                throw new OperationException(OperationException.CONFIRMATION_REJECTED,
                        "Only the requesting principal and device may confirm");
            }
            return issueIn(c, actor, op, sourceRevision, ttl);
        });
    }

    static boolean validTtl(Duration ttl) {
        return ttl != null && !ttl.isNegative() && !ttl.isZero() && ttl.compareTo(MAX_TTL) <= 0;
    }

    /**
     * Inserts a confirmation for {@code op} (as loaded in {@code c}) and appends its audit record in the
     * caller's transaction. The caller has already validated state, ownership, revision and ttl.
     */
    IssuedConfirmation issueIn(Connection c, AuditActor actor, Operation op, String sourceRevision, Duration ttl)
            throws SQLException {
        byte[] raw = new byte[32];
        RANDOM.nextBytes(raw);
        String token = Base64.getUrlEncoder().withoutPadding().encodeToString(raw);
        String id = UUID.randomUUID().toString();
        long now = db.clock().millis();
        Instant expires = Instant.ofEpochMilli(now).plus(ttl);
        try (PreparedStatement ps = c.prepareStatement(
                "INSERT INTO confirmations (id, server_id, operation_id, actor_principal_id, device_id, action, "
                        + "target, payload_hash, source_revision, token_hash, created_at, expires_at) "
                        + "VALUES (?,?,?,?,?,?,?,?,?,?,?,?)")) {
            ps.setString(1, id);
            ps.setString(2, db.serverId());
            ps.setString(3, op.id());
            ps.setString(4, actor.principalId());
            ps.setString(5, actor.deviceId());
            ps.setString(6, op.type());
            ps.setString(7, op.target());
            ps.setString(8, op.payloadHash());
            ps.setString(9, sourceRevision);
            ps.setString(10, StateValues.sha256Hex(token));
            ps.setLong(11, now);
            ps.setLong(12, expires.toEpochMilli());
            ps.executeUpdate();
        }
        audit.append(c, new AuditRecord(actor, "confirmation.issue", op.target(), "ok", op.id(),
                Map.of("confirmationId", id, "sourceRevision", sourceRevision, "expiresAt", expires.toString())));
        return new IssuedConfirmation(id, token, op.id(), actor.principalId(), actor.deviceId(), op.type(),
                op.target(), op.payloadHash(), sourceRevision, expires);
    }

    /**
     * Trusted confirm: atomically consumes the confirmation and queues the operation. See the class
     * documentation for where the expected values must come from.
     *
     * @throws OperationException {@code confirmation_rejected} on any mismatch, reuse or expiry
     */
    public Operation confirm(AuditActor actor, String operationId, String token, String action, String target,
                             String payloadHash, String sourceRevision) {
        Objects.requireNonNull(actor, "actor");
        if (!wellFormedToken(token)) {
            throw reject(actor, operationId, target, "malformed_token", null);
        }
        String tokenHash = StateValues.sha256Hex(token);
        Expectation fixed = new Expectation(action, target, payloadHash, sourceRevision, null);
        // Accept or reject in ONE write transaction: a rejection burns the token and is audited in the
        // same commit, so no concurrent exact confirmation can slip in between. Throw after commit.
        Outcome outcome = db.write(c -> consumeIn(c, actor, operationId, tokenHash, target, (cc, op) -> fixed));
        if (outcome.rejection() != null) {
            throw rejected(outcome.rejection());
        }
        return outcome.operation();
    }

    static boolean wellFormedToken(String token) {
        return token != null && !token.isEmpty() && token.length() <= MAX_TOKEN_CHARS;
    }

    /** Result of an in-transaction consume attempt: exactly one of the fields is non-null. */
    record Outcome(Operation operation, String rejection) {
    }

    /**
     * Exact values the stored binding must match, or a {@code denial} reason. Produced after the token's
     * binding has passed its token/expiry/operation/actor checks, so no policy or state detail is evaluated
     * for unknown or foreign tokens.
     */
    record Expectation(String action, String target, String payloadHash, String sourceRevision, String denial) {
        static Expectation denied(String reason) {
            return new Expectation(null, null, null, null, reason == null ? "denied" : reason);
        }
    }

    @FunctionalInterface
    interface ExpectationSource {
        /** {@code current} is the operation as loaded in {@code c}, or null when it does not exist. */
        Expectation expect(Connection c, Operation current) throws SQLException;
    }

    /**
     * Consumes the confirmation for {@code tokenHash} inside the caller's write transaction. On success the
     * operation is moved to queued and audited; on any rejection the presented token is burned (if live)
     * and the rejection audited in the same transaction, and the reason is returned (not thrown) so the
     * caller can commit before reporting it. If an audit append fails the exception propagates and the
     * caller's transaction rolls back every mutation.
     *
     * @param auditTarget target to record on rejection; when null the loaded operation's target (or
     *                    "unknown") is used
     */
    Outcome consumeIn(Connection c, AuditActor actor, String operationId, String tokenHash, String auditTarget,
                      ExpectationSource expectations) throws SQLException {
        Binding b = findByTokenHash(c, tokenHash);
        String reason = tokenMismatch(b, actor, operationId);
        Operation op = null;
        if (reason == null) {
            op = operations.load(c, operationId).orElse(null);
            Expectation expected = expectations.expect(c, op);
            reason = expected == null ? "denied" : expected.denial() != null ? expected.denial()
                    : valueMismatch(b, expected);
        }
        if (reason == null) {
            if (op == null || op.state() != OperationState.AWAITING_CONFIRMATION || !op.type().equals(b.action)
                    || !op.target().equals(b.target) || !op.payloadHash().equals(b.payloadHash)) {
                reason = "operation_changed";
            }
        }
        if (reason == null) {
            try (PreparedStatement ps = c.prepareStatement(
                    "UPDATE confirmations SET consumed_at = ? WHERE id = ? AND consumed_at IS NULL AND expires_at > ?")) {
                long now = db.clock().millis();
                ps.setLong(1, now);
                ps.setString(2, b.id);
                ps.setLong(3, now);
                if (ps.executeUpdate() != 1) {
                    reason = "already_consumed";
                }
            }
        }
        if (reason != null) {
            String target = auditTarget != null ? auditTarget : op != null ? op.target() : null;
            rejectInTransaction(c, actor, operationId, target, reason, tokenHash);
            return new Outcome(null, reason);
        }
        operations.setState(c, operationId, OperationState.QUEUED);
        Operation after = operations.load(c, operationId).orElseThrow();
        operations.record(c, actor, "confirmation.consume", after, OperationState.AWAITING_CONFIRMATION,
                Map.of("confirmationId", b.id, "sourceRevision", b.sourceRevision));
        return new Outcome(after, null);
    }

    // ------------------------------------------------------------------ internals

    private record Binding(String id, String operationId, String principalId, String deviceId, String action,
                           String target, String payloadHash, String sourceRevision, long expiresAt,
                           boolean consumed) {
    }

    private String tokenMismatch(Binding b, AuditActor actor, String operationId) {
        if (b == null) {
            return "unknown_token";
        }
        if (b.consumed) {
            return "already_consumed";
        }
        if (db.clock().millis() >= b.expiresAt) {
            return "expired";
        }
        if (!b.operationId.equals(operationId)) {
            return "operation_mismatch";
        }
        if (!b.principalId.equals(actor.principalId()) || !b.deviceId.equals(actor.deviceId())) {
            return "actor_mismatch";
        }
        return null;
    }

    private static String valueMismatch(Binding b, Expectation expected) {
        if (!b.action.equals(expected.action())) {
            return "action_mismatch";
        }
        if (!b.target.equals(expected.target())) {
            return "target_mismatch";
        }
        if (!b.payloadHash.equals(expected.payloadHash())) {
            return "payload_mismatch";
        }
        if (!b.sourceRevision.equals(expected.sourceRevision())) {
            return "stale_source_revision";
        }
        return null;
    }

    private static Binding findByTokenHash(Connection c, String tokenHash) throws SQLException {
        try (PreparedStatement ps = c.prepareStatement(
                "SELECT id, operation_id, actor_principal_id, device_id, action, target, payload_hash, source_revision, "
                        + "expires_at, consumed_at FROM confirmations WHERE token_hash = ?")) {
            ps.setString(1, tokenHash);
            try (ResultSet rs = ps.executeQuery()) {
                if (!rs.next()) {
                    return null;
                }
                return new Binding(rs.getString(1), rs.getString(2), rs.getString(3), rs.getString(4),
                        rs.getString(5), rs.getString(6), rs.getString(7), rs.getString(8), rs.getLong(9),
                        rs.getObject(10) != null);
            }
        }
    }

    /** Audits a rejection that has no token to burn, in its own transaction. */
    private OperationException reject(AuditActor actor, String operationId, String target, String reason,
                                      String tokenHash) {
        db.write(c -> {
            rejectInTransaction(c, actor, operationId, target, reason, tokenHash);
            return null;
        });
        return rejected(reason);
    }

    static OperationException rejected(String reason) {
        return new OperationException(OperationException.CONFIRMATION_REJECTED, "Confirmation rejected: " + reason);
    }

    /** Burns the presented token (if live) and appends the rejection audit inside the caller's transaction. */
    void rejectInTransaction(Connection c, AuditActor actor, String operationId, String target, String reason,
                             String tokenHash) throws SQLException {
        String safeTarget = safe(target, 512, "unknown");
        String safeOp = safe(operationId, 64, "");
        if (tokenHash != null) {
            try (PreparedStatement ps = c.prepareStatement(
                    "UPDATE confirmations SET consumed_at = ? WHERE token_hash = ? AND consumed_at IS NULL")) {
                ps.setLong(1, db.clock().millis());
                ps.setString(2, tokenHash);
                ps.executeUpdate();
            }
        }
        audit.append(c, new AuditRecord(actor, "confirmation.consume", safeTarget, "rejected", null,
                Map.of("reason", safe(reason, 64, "denied"), "operation", safeOp)));
    }

    static String safe(String value, int max, String fallback) {
        if (value == null || value.isBlank()) {
            return fallback;
        }
        String bounded = StateValues.bounded(value, max);
        return bounded.chars().anyMatch(Character::isISOControl) ? fallback : bounded;
    }
}
