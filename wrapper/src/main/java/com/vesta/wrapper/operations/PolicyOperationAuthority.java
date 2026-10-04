package com.vesta.wrapper.operations;

import com.vesta.wrapper.authorization.AuthorizationService;

import java.sql.Connection;
import java.sql.SQLException;
import java.util.List;
import java.util.Objects;

/**
 * Canonical {@link OperationAuthority} for the runner: at every stage it re-derives the operation's
 * binding from the stored canonical input and re-resolves the actor's current policy from storage.
 *
 * <p>Denies (fail closed) when: the operation belongs to another server; the type has no trusted
 * binding; the input is missing, purged or fails its hash binding; the stored input is not in canonical
 * form; the derived target, lock resources or contract flags differ from the persisted operation; the
 * principal/device is inactive, revoked, expired or bound to another server; any requirement is denied
 * (explicit denies and reserved tiers included); or any evaluation throws.
 *
 * <p>Operations submitted by {@link com.vesta.wrapper.audit.AuditActor#system} or other non-principal
 * actors resolve to no identity and are therefore denied here; trusted system operations need their own
 * explicitly reviewed authority.
 *
 * <p>{@link #recheck} must be called outside StateDatabase callbacks (the runner does so); it evaluates in
 * one read snapshot. In-transaction callers (e.g. {@link PolicyConfirmationService}) use the package-private
 * {@code verify(Connection, Operation)}, which performs exactly the same checks on the caller's connection.
 */
public final class PolicyOperationAuthority implements OperationAuthority {
    private final OperationService ops;
    private final AuthorizationService authorization;
    private final OperationBindings bindings;

    public PolicyOperationAuthority(OperationService ops, AuthorizationService authorization,
                                    OperationBindings bindings) {
        this.ops = Objects.requireNonNull(ops, "ops");
        this.authorization = Objects.requireNonNull(authorization, "authorization");
        this.bindings = Objects.requireNonNull(bindings, "bindings");
    }

    @Override
    public Decision recheck(Operation operation, Stage stage) {
        if (stage == null) {
            return Decision.deny("no_stage");
        }
        try {
            // One read snapshot: stored input, binding integrity and current policy are evaluated together.
            String reason = ops.database().read(c -> verify(c, operation).denial());
            return reason == null ? Decision.allow() : Decision.deny(reason);
        } catch (RuntimeException e) {
            return Decision.deny("authority_check_failed");
        }
    }

    /**
     * Outcome of the canonical check: {@code denial} is null when allowed, in which case
     * {@code canonicalInput} is the hash-verified stored canonical input the decision was bound to.
     */
    record Verdict(String denial, String canonicalInput) {
        static Verdict deny(String reason) {
            return new Verdict(reason == null ? "denied" : reason, null);
        }
    }

    /**
     * Canonical fail-closed check inside the caller's transaction (no nested reads/writes): server, trusted
     * binding, hash-verified stored input, canonical re-derivation equal to the persisted operation, and the
     * operation principal/device's <em>current</em> policy for every derived requirement. Runtime failures
     * deny with {@code authority_check_failed}; SQL failures propagate so the caller's transaction aborts.
     */
    Verdict verify(Connection c, Operation op) throws SQLException {
        try {
            return verifyUnchecked(c, op);
        } catch (SQLException e) {
            throw e;
        } catch (RuntimeException e) {
            return Verdict.deny("authority_check_failed");
        }
    }

    private Verdict verifyUnchecked(Connection c, Operation op) throws SQLException {
        if (op == null) {
            return Verdict.deny("no_operation");
        }
        if (!ops.database().serverId().equals(op.serverId())) {
            return Verdict.deny("wrong_server");
        }
        OperationBinding<?> binding = bindings.find(op.type()).orElse(null);
        if (binding == null) {
            return Verdict.deny("unbound_type");
        }
        OperationInput input = ops.loadInput(c, op.id()).orElse(null);
        if (input == null || input.status() != OperationInput.InputStatus.AVAILABLE) {
            return Verdict.deny(OperationService.INPUT_UNAVAILABLE);
        }
        // loadInput() already verified the stored JSON against the stored row hash; additionally bind it
        // to the hash of the operation snapshot being authorized.
        if (op.payloadHash() == null || !op.payloadHash().equals(input.payloadHash())) {
            return Verdict.deny("payload_mismatch");
        }
        OperationBinding.Derived derived;
        try {
            derived = binding.derive(input.canonicalJson());
        } catch (OperationFailure f) {
            return Verdict.deny(f.getCode());
        }
        if (!derived.canonicalInput().equals(input.canonicalJson())
                || !derived.target().equals(op.target())
                || !derived.lockResources().equals(List.copyOf(op.lockResources()))
                || derived.contract().externalEffectIdempotent() != op.externalEffectIdempotent()
                || derived.contract().requiresConfirmation() != op.requiresConfirmation()) {
            return Verdict.deny("binding_mismatch");
        }
        String denial = OperationPolicy.denyReason(
                authorization.resolve(c, op.actorPrincipalId(), op.deviceId()),
                op.actorPrincipalId(), op.deviceId(), derived.requirements());
        return denial == null ? new Verdict(null, input.canonicalJson()) : Verdict.deny(denial);
    }
}
