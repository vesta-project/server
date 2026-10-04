package com.vesta.wrapper.operations;

import com.vesta.wrapper.audit.AuditActor;
import com.vesta.wrapper.audit.AuditLog;
import com.vesta.wrapper.audit.AuditRecord;
import com.vesta.wrapper.authorization.AuthorizationService;
import com.vesta.wrapper.identity.IdentityRecords.RemoteActor;
import com.vesta.wrapper.state.StateDatabase;
import com.vesta.wrapper.state.StateValues;

import java.sql.Connection;
import java.sql.SQLException;
import java.time.Duration;
import java.util.Map;
import java.util.Objects;

/**
 * Canonical confirmation gateway for principal-initiated (remote) confirmations.
 *
 * <p>Callers supply only the authenticated actor (from the device transport, never from request bodies,
 * headers, loopback location or the {@code source} label), an audit source label, the operation id and
 * either a TTL (issue) or the one-time token (confirm). Action, target, payload hash and source revision
 * are never accepted from the caller: they come from the stored operation and the trusted
 * {@link SourceRevisionProvider}.
 *
 * <p>Issue and confirm each run in ONE StateDatabase write transaction that also contains: loading the
 * operation, requiring it to await confirmation and belong to the exact requesting principal and device,
 * the canonical {@link PolicyOperationAuthority} check (trusted binding, hash-verified stored canonical
 * input, re-derived target/locks/contract equal to the persisted operation, and the principal/device's
 * <em>current</em> policy for every exact derived capability), the trusted source revision, the
 * confirmation insert/consume and the audit. No nested reads or writes are performed.
 *
 * <p>Fail closed: null/malformed actors or labels, {@code system} actors, malformed ids/tokens/TTLs,
 * unknown/unbound/stale operations, policy denials and provider failures are rejected with a bounded,
 * secret-free audited reason. A confirmation rejection burns the presented token (if live) in the same
 * transaction. Rejections are thrown only after the audit commits; if auditing fails, nothing commits.
 * The source label never grants authority and is not part of the confirmation binding.
 */
public final class PolicyConfirmationService {
    static final String GATEWAY_SOURCE = "confirmation-gateway";
    private static final int MAX_OPERATION_ID_CHARS = 64;
    private static final int MAX_REVISION_CHARS = 128;

    /**
     * Trusted server-owned view of the operation target's current revision (e.g. live game generation).
     *
     * <p>Called inside the gateway's write transaction, only after binding integrity and current policy
     * passed, with the operation as loaded in that transaction and its hash-verified stored canonical input
     * (exactly what will execute). It must not call {@link StateDatabase#read}/{@link StateDatabase#write}
     * (use {@code c} if state is needed) and should be fast and non-blocking. Returning null, a blank,
     * over-long (&gt;128) or control-character value, or throwing anything (including {@link SQLException}), denies with
     * {@code source_revision_unavailable}; return null to refuse a stale target.
     */
    @FunctionalInterface
    public interface SourceRevisionProvider {
        String sourceRevision(Connection c, Operation operation, String canonicalInput) throws SQLException;
    }

    private final OperationService ops;
    private final ConfirmationService confirmations;
    private final PolicyOperationAuthority authority;
    private final SourceRevisionProvider revisions;
    private final StateDatabase db;
    private final AuditLog audit;

    public PolicyConfirmationService(OperationService ops, ConfirmationService confirmations,
                                     AuthorizationService authorization, OperationBindings bindings,
                                     SourceRevisionProvider revision) {
        this.ops = Objects.requireNonNull(ops, "ops");
        this.confirmations = Objects.requireNonNull(confirmations, "confirmations");
        this.authority = new PolicyOperationAuthority(ops, Objects.requireNonNull(authorization, "authorization"),
                Objects.requireNonNull(bindings, "bindings"));
        this.revisions = Objects.requireNonNull(revision, "revision");
        this.db = ops.database();
        if (confirmations.database() != db || confirmations.operations() != ops) {
            throw new IllegalArgumentException("confirmations must share the operation service and database");
        }
        this.audit = Objects.requireNonNull(confirmations.audit(), "audit");
    }

    /**
     * Issues a single-use confirmation for an awaiting-confirmation operation owned by {@code actor}.
     *
     * @param source audit label for the request channel (e.g. "https"); confers no authority
     * @param ttl    must satisfy {@code 0 < ttl <= 15m}
     * @throws OperationException {@code invalid_request} for an out-of-range ttl; otherwise
     *         {@code confirmation_rejected} with a stable reason (malformed input, wrong state/owner, binding
     *         integrity or current-policy denial, stale/unavailable revision)
     */
    public ConfirmationService.IssuedConfirmation issue(RemoteActor actor, String source, String operationId,
                                                        Duration ttl) {
        Caller caller = Caller.of(actor, source);
        IssueOutcome outcome = db.write(c -> {
            String early = caller.rejection != null ? caller.rejection
                    : !validOperationId(operationId) ? "malformed_operation"
                    : !ConfirmationService.validTtl(ttl) ? "invalid_ttl" : null;
            if (early != null) {
                auditIssueRejection(c, caller.audit, operationId, null, early);
                return IssueOutcome.rejected(early);
            }
            Operation op = ops.load(c, operationId).orElse(null);
            Check check = check(c, caller.audit, op);
            if (check.denial != null) {
                auditIssueRejection(c, caller.audit, operationId, op == null ? null : op.target(), check.denial);
                return IssueOutcome.rejected(check.denial);
            }
            return new IssueOutcome(confirmations.issueIn(c, caller.audit, op, check.revision, ttl), null);
        });
        if (outcome.rejection != null) {
            throw failure(outcome.rejection);
        }
        return outcome.issued;
    }

    /**
     * Consumes {@code token} and queues the operation. Expected action/target/payload hash come from the
     * stored operation and the source revision from the trusted provider; all must equal the issued binding.
     *
     * @param source audit label for the request channel; confers no authority
     * @throws OperationException {@code confirmation_rejected} for every rejection (the token is burned)
     */
    public Operation confirm(RemoteActor actor, String source, String operationId, String token) {
        Caller caller = Caller.of(actor, source);
        String tokenHash = ConfirmationService.wellFormedToken(token) ? StateValues.sha256Hex(token) : null;
        ConfirmationService.Outcome outcome = db.write(c -> {
            String early = caller.rejection != null ? caller.rejection
                    : tokenHash == null ? "malformed_token"
                    : !validOperationId(operationId) ? "malformed_operation" : null;
            if (early != null) {
                // Burns the presented token (if well formed and live) in this same transaction.
                confirmations.rejectInTransaction(c, caller.audit, operationId, null, early, tokenHash);
                return new ConfirmationService.Outcome(null, early);
            }
            return confirmations.consumeIn(c, caller.audit, operationId, tokenHash, null, (cc, op) -> {
                Check check = check(cc, caller.audit, op);
                return check.denial != null ? ConfirmationService.Expectation.denied(check.denial)
                        : new ConfirmationService.Expectation(op.type(), op.target(), op.payloadHash(),
                        check.revision, null);
            });
        });
        if (outcome.rejection() != null) {
            throw ConfirmationService.rejected(outcome.rejection());
        }
        return outcome.operation();
    }

    // ------------------------------------------------------------------ internals

    private record IssueOutcome(ConfirmationService.IssuedConfirmation issued, String rejection) {
        static IssueOutcome rejected(String reason) {
            return new IssueOutcome(null, reason);
        }
    }

    private record Check(String denial, String revision) {
        static Check deny(String reason) {
            return new Check(reason == null ? "denied" : reason, null);
        }
    }

    /**
     * Validated caller. {@code audit} is always usable for auditing; when the actor or label is unusable it
     * is a gateway marker actor and {@code rejection} is set, so it never authorizes anything.
     */
    private record Caller(AuditActor audit, String rejection) {
        static Caller of(RemoteActor actor, String source) {
            if (actor == null) {
                return new Caller(AuditActor.system(GATEWAY_SOURCE), "no_actor");
            }
            AuditActor auditActor;
            try {
                auditActor = new AuditActor(actor.principalId(), actor.deviceId(), GATEWAY_SOURCE);
            } catch (RuntimeException e) {
                return new Caller(AuditActor.system(GATEWAY_SOURCE), "malformed_actor");
            }
            if (AuditActor.SYSTEM_PRINCIPAL.equals(actor.principalId())) {
                return new Caller(auditActor, "system_actor");
            }
            try {
                return new Caller(new AuditActor(actor.principalId(), actor.deviceId(), source), null);
            } catch (RuntimeException e) {
                return new Caller(auditActor, "malformed_source");
            }
        }
    }

    /** Ownership, state, canonical binding/policy and trusted revision for {@code op} in {@code c}. */
    private Check check(Connection c, AuditActor actor, Operation op) throws SQLException {
        if (op == null) {
            return Check.deny("unknown_operation");
        }
        if (op.state() != OperationState.AWAITING_CONFIRMATION || !op.requiresConfirmation()) {
            return Check.deny("not_awaiting_confirmation");
        }
        if (!op.actorPrincipalId().equals(actor.principalId()) || !op.deviceId().equals(actor.deviceId())) {
            return Check.deny("actor_mismatch");
        }
        PolicyOperationAuthority.Verdict verdict = authority.verify(c, op);
        if (verdict.denial() != null) {
            return Check.deny(verdict.denial());
        }
        String revision;
        try {
            revision = revisions.sourceRevision(c, op, verdict.canonicalInput());
        } catch (SQLException | RuntimeException e) {
            // Any provider failure denies; no detail escapes. If it left the connection unusable, the
            // refusal audit fails next and the whole transaction rolls back (still fail closed).
            revision = null;
        }
        if (!validRevision(revision)) {
            return Check.deny("source_revision_unavailable");
        }
        return new Check(null, revision);
    }

    private void auditIssueRejection(Connection c, AuditActor actor, String operationId, String target,
                                     String reason) throws SQLException {
        audit.append(c, new AuditRecord(actor, "confirmation.issue", ConfirmationService.safe(target, 512, "unknown"),
                "rejected", null, Map.of("reason", ConfirmationService.safe(reason, 64, "denied"),
                "operation", ConfirmationService.safe(operationId, MAX_OPERATION_ID_CHARS, ""))));
    }

    private static OperationException failure(String reason) {
        if ("invalid_ttl".equals(reason)) {
            return new OperationException(OperationException.INVALID_REQUEST, "confirmation ttl must be within 0..15m");
        }
        return ConfirmationService.rejected(reason);
    }

    private static boolean validOperationId(String id) {
        return id != null && !id.isBlank() && id.length() <= MAX_OPERATION_ID_CHARS
                && id.chars().noneMatch(Character::isISOControl);
    }

    private static boolean validRevision(String revision) {
        return revision != null && !revision.isBlank() && revision.length() <= MAX_REVISION_CHARS
                && revision.chars().noneMatch(Character::isISOControl);
    }
}
