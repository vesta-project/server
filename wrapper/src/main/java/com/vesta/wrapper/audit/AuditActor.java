package com.vesta.wrapper.audit;

import com.vesta.wrapper.state.StateValues;

/** Who performed an action: principal, device and request source (e.g. "https", "local-cli", "operation-engine"). */
public record AuditActor(String principalId, String deviceId, String source) {
    public static final String SYSTEM_PRINCIPAL = "system";
    public static final String LOCAL_DEVICE = "local";

    public AuditActor {
        StateValues.requireText("principalId", principalId, 128);
        StateValues.requireText("deviceId", deviceId, 128);
        StateValues.requireText("source", source, 64);
    }

    /** The agent itself acting (e.g. the operation engine or recovery). */
    public static AuditActor system(String source) {
        return new AuditActor(SYSTEM_PRINCIPAL, LOCAL_DEVICE, source);
    }
}
