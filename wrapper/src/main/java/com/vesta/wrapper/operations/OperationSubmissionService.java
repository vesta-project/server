package com.vesta.wrapper.operations;

import com.vesta.wrapper.audit.AuditActor;
import com.vesta.wrapper.authorization.AuthorizationService;
import com.vesta.wrapper.identity.IdentityRecords.RemoteActor;

import java.util.List;
import java.util.Objects;

/**
 * Trusted submission boundary for principal-initiated operations.
 *
 * <p>The caller supplies only the authenticated actor (from the device transport, never from request
 * bodies, headers, loopback location or the {@code source} label), the operation type, the raw typed
 * input and an optional idempotency key. Everything else — canonical input, target, lock resources,
 * idempotency/confirmation flags and capability requirements — is derived from the trusted
 * {@link OperationBinding}. Current policy is resolved and checked inside the same write transaction that
 * inserts the operation and its audit; a denial is audited in that transaction and then thrown.
 *
 * <p>Pre-derivation rejections (unknown type, invalid input/target) are also audited as rejected
 * submissions when the actor and type are well formed.
 */
public final class OperationSubmissionService {
    private final OperationService ops;
    private final AuthorizationService authorization;
    private final OperationBindings bindings;

    public OperationSubmissionService(OperationService ops, AuthorizationService authorization,
                                      OperationBindings bindings) {
        this.ops = Objects.requireNonNull(ops, "ops");
        this.authorization = Objects.requireNonNull(authorization, "authorization");
        this.bindings = Objects.requireNonNull(bindings, "bindings");
    }

    /**
     * @param source audit label for the request channel (e.g. "https"); confers no authority
     * @throws OperationException {@code unknown_type}, {@code invalid_request} (malformed input/target/
     *         requirements/resources), {@code authorization_denied}, {@code idempotency_conflict}
     */
    public SubmitResult submit(RemoteActor actor, String source, String type, String rawInput, String idempotencyKey) {
        if (actor == null) {
            throw new OperationException(OperationException.AUTHORIZATION_DENIED, "No authenticated actor");
        }
        AuditActor auditActor = new AuditActor(actor.principalId(), actor.deviceId(), source);
        OperationBinding<?> binding = bindings.find(type).orElse(null);
        if (binding == null) {
            auditRejected(auditActor, type, OperationException.UNKNOWN_TYPE);
            throw new OperationException(OperationException.UNKNOWN_TYPE, "No trusted binding for operation type");
        }
        OperationBinding.Derived derived;
        try {
            derived = binding.derive(rawInput);
        } catch (OperationFailure f) {
            auditRejected(auditActor, type, f.getCode());
            throw new OperationException(OperationException.INVALID_REQUEST, "Operation input rejected: " + f.getCode());
        }
        OperationRequest request = new OperationRequest(auditActor, derived.type(), derived.target(),
                derived.canonicalInput(), idempotencyKey, derived.lockResources(),
                derived.contract().externalEffectIdempotent(), derived.contract().requiresConfirmation());
        List<OperationBinding.Requirement> requirements = derived.requirements();
        return ops.submitGuarded(request, c -> OperationPolicy.denyReason(
                authorization.resolve(c, actor.principalId(), actor.deviceId()),
                actor.principalId(), actor.deviceId(), requirements));
    }

    /** Audits a rejection that happened before a target could be derived (no operation row). */
    private void auditRejected(AuditActor actor, String type, String reason) {
        OperationRequest marker;
        try {
            marker = new OperationRequest(actor, type, "unbound", "", null, List.of(), false, false);
        } catch (RuntimeException e) {
            throw new OperationException(OperationException.INVALID_REQUEST, "Malformed operation type");
        }
        try {
            ops.submitGuarded(marker, c -> reason);
        } catch (OperationException expected) {
            if (!OperationException.AUTHORIZATION_DENIED.equals(expected.getCode())) {
                throw expected;
            }
        }
    }
}
