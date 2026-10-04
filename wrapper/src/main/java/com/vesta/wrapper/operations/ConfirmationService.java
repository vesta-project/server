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
 * <p>{@code sourceRevision} must come from the server's own view of the target state at issue and
 * at confirm time (not from the client), so stale state is rejected.
 */
public final class ConfirmationService {
    public static final Duration MAX_TTL = Duration.ofMinutes(15);
    private static final SecureRandom RANDOM = new SecureRandom();

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

    public IssuedConfirmation issue(AuditActor actor, String operationId, String sourceRevision, Duration ttl) {
        Objects.requireNonNull(actor, "actor");
        StateValues.requireText("sourceRevision", sourceRevision, 128);
        if (ttl == null || ttl.isNegative() || ttl.isZero() || ttl.compareTo(MAX_TTL) > 0) {
            throw new OperationException(OperationException.INVALID_REQUEST, "confirmation ttl must be within 0..15m");
        }
        byte[] raw = new byte[32];
        RANDOM.nextBytes(raw);
        String token = Base64.getUrlEncoder().withoutPadding().encodeToString(raw);
        return db.write(c -> {
            Operation op = operations.require(c, operationId);
            if (op.state() != OperationState.AWAITING_CONFIRMATION) {
                throw new OperationException(OperationException.INVALID_TRANSITION, "Operation is not awaiting confirmation");
            }
            if (!op.actorPrincipalId().equals(actor.principalId()) || !op.deviceId().equals(actor.deviceId())) {
                throw new OperationException(OperationException.CONFIRMATION_REJECTED,
                        "Only the requesting principal and device may confirm");
            }
            String id = UUID.randomUUID().toString();
            long now = db.clock().millis();
            Instant expires = Instant.ofEpochMilli(now).plus(ttl);
            try (PreparedStatement ps = c.prepareStatement(
                    "INSERT INTO confirmations (id, server_id, operation_id, actor_principal_id, device_id, action, "
                            + "target, payload_hash, source_revision, token_hash, created_at, expires_at) "
                            + "VALUES (?,?,?,?,?,?,?,?,?,?,?,?)")) {
                ps.setString(1, id);
                ps.setString(2, db.serverId());
                ps.setString(3, operationId);
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
            audit.append(c, new AuditRecord(actor, "confirmation.issue", op.target(), "ok", operationId,
                    Map.of("confirmationId", id, "sourceRevision", sourceRevision, "expiresAt", expires.toString())));
            return new IssuedConfirmation(id, token, operationId, actor.principalId(), actor.deviceId(), op.type(),
                    op.target(), op.payloadHash(), sourceRevision, expires);
        });
    }

    /**
     * Atomically consumes the confirmation and queues the operation.
     *
     * @throws OperationException {@code confirmation_rejected} on any mismatch, reuse or expiry
     */
    public Operation confirm(AuditActor actor, String operationId, String token, String action, String target,
                             String payloadHash, String sourceRevision) {
        Objects.requireNonNull(actor, "actor");
        if (token == null || token.isEmpty() || token.length() > 128) {
            throw reject(actor, operationId, target, "malformed_token", null);
        }
        String tokenHash = StateValues.sha256Hex(token);
        // Accept or reject in ONE write transaction: a rejection burns the token and is audited in the
        // same commit, so no concurrent exact confirmation can slip in between. Throw after commit.
        Outcome outcome = db.write(c -> {
            Binding b = findByTokenHash(c, tokenHash);
            String reason = mismatch(b, actor, operationId, action, target, payloadHash, sourceRevision);
            if (reason == null) {
                Operation op = operations.load(c, operationId).orElse(null);
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
                rejectInTransaction(c, actor, operationId, target, reason, tokenHash);
                return new Outcome(null, reason);
            }
            operations.setState(c, operationId, OperationState.QUEUED);
            Operation after = operations.load(c, operationId).orElseThrow();
            operations.record(c, actor, "confirmation.consume", after, OperationState.AWAITING_CONFIRMATION,
                    Map.of("confirmationId", b.id, "sourceRevision", b.sourceRevision));
            return new Outcome(after, null);
        });
        if (outcome.rejection() != null) {
            throw rejected(outcome.rejection());
        }
        return outcome.operation();
    }

    private record Outcome(Operation operation, String rejection) {
    }

    // ------------------------------------------------------------------ internals

    private record Binding(String id, String operationId, String principalId, String deviceId, String action,
                           String target, String payloadHash, String sourceRevision, long expiresAt,
                           boolean consumed) {
    }

    private String mismatch(Binding b, AuditActor actor, String operationId, String action, String target,
                            String payloadHash, String sourceRevision) {
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
        if (!b.action.equals(action)) {
            return "action_mismatch";
        }
        if (!b.target.equals(target)) {
            return "target_mismatch";
        }
        if (!b.payloadHash.equals(payloadHash)) {
            return "payload_mismatch";
        }
        if (!b.sourceRevision.equals(sourceRevision)) {
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
                Binding b = new Binding(rs.getString(1), rs.getString(2), rs.getString(3), rs.getString(4),
                        rs.getString(5), rs.getString(6), rs.getString(7), rs.getString(8), rs.getLong(9),
                        rs.getObject(10) != null);
                return b;
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

    private static OperationException rejected(String reason) {
        return new OperationException(OperationException.CONFIRMATION_REJECTED, "Confirmation rejected: " + reason);
    }

    /** Burns the presented token (if live) and appends the rejection audit inside the caller's transaction. */
    private void rejectInTransaction(Connection c, AuditActor actor, String operationId, String target, String reason,
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
                Map.of("reason", reason, "operation", safeOp)));
    }

    private static String safe(String value, int max, String fallback) {
        if (value == null || value.isBlank()) {
            return fallback;
        }
        String bounded = StateValues.bounded(value, max);
        return bounded.chars().anyMatch(Character::isISOControl) ? fallback : bounded;
    }
}
