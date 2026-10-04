package com.vesta.wrapper.operations;

import com.vesta.wrapper.authorization.Decision;
import com.vesta.wrapper.authorization.EffectivePolicy;

import java.util.List;

/** Shared fail-closed evaluation of binding requirements against a freshly resolved policy. */
final class OperationPolicy {
    private OperationPolicy() {
    }

    /** Returns null when every requirement is allowed, otherwise a stable deny reason. */
    static String denyReason(EffectivePolicy policy, String principalId, String deviceId,
                             List<OperationBinding.Requirement> requirements) {
        try {
            if (policy == null || !policy.isAuthenticated()) {
                return Decision.NO_IDENTITY;
            }
            if (!policy.principalId().equals(principalId) || !policy.deviceId().equals(deviceId)) {
                return "identity_mismatch";
            }
            if (requirements == null || requirements.isEmpty()
                    || requirements.size() > OperationBinding.MAX_REQUIREMENTS) {
                return "invalid_requirements";
            }
            for (OperationBinding.Requirement r : requirements) {
                Decision d = policy.check(r.capability(), r.resource());
                if (d == null || !d.allowed()) {
                    return d == null || d.reason() == null ? "denied" : d.reason();
                }
            }
            return null;
        } catch (RuntimeException e) {
            return "authority_check_failed";
        }
    }
}
