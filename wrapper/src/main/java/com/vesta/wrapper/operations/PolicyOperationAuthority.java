package com.vesta.wrapper.operations;

import com.vesta.wrapper.authorization.AuthorizationService;

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
 * <p>Must be called outside StateDatabase callbacks (the runner does so).
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
            String reason = denyReason(operation);
            return reason == null ? Decision.allow() : Decision.deny(reason);
        } catch (RuntimeException e) {
            return Decision.deny("authority_check_failed");
        }
    }

    private String denyReason(Operation op) {
        if (op == null) {
            return "no_operation";
        }
        if (!ops.database().serverId().equals(op.serverId())) {
            return "wrong_server";
        }
        OperationBinding<?> binding = bindings.find(op.type()).orElse(null);
        if (binding == null) {
            return "unbound_type";
        }
        OperationInput input = ops.input(op.id()).orElse(null);
        if (input == null || input.status() != OperationInput.InputStatus.AVAILABLE) {
            return OperationService.INPUT_UNAVAILABLE;
        }
        // ops.input() already verified the stored JSON against the stored row hash; additionally bind it
        // to the hash of the operation snapshot being authorized.
        if (op.payloadHash() == null || !op.payloadHash().equals(input.payloadHash())) {
            return "payload_mismatch";
        }
        OperationBinding.Derived derived;
        try {
            derived = binding.derive(input.canonicalJson());
        } catch (OperationFailure f) {
            return f.getCode();
        }
        if (!derived.canonicalInput().equals(input.canonicalJson())
                || !derived.target().equals(op.target())
                || !derived.lockResources().equals(List.copyOf(op.lockResources()))
                || derived.contract().externalEffectIdempotent() != op.externalEffectIdempotent()
                || derived.contract().requiresConfirmation() != op.requiresConfirmation()) {
            return "binding_mismatch";
        }
        return OperationPolicy.denyReason(
                authorization.resolve(op.actorPrincipalId(), op.deviceId()),
                op.actorPrincipalId(), op.deviceId(), derived.requirements());
    }
}
