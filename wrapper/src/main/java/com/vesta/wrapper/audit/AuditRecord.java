package com.vesta.wrapper.audit;

import com.vesta.wrapper.state.StateException;
import com.vesta.wrapper.state.StateValues;

import java.util.Locale;
import java.util.Map;
import java.util.Objects;
import java.util.TreeMap;

/**
 * An audit record to append. Metadata is bounded and must be safe: keys that look like
 * secrets are rejected so callers cannot accidentally persist credentials.
 */
public record AuditRecord(AuditActor actor, String action, String target, String outcome,
                          String operationId, Map<String, String> metadata) {
    public static final int MAX_METADATA_ENTRIES = 16;
    public static final int MAX_METADATA_VALUE = 256;
    private static final String[] FORBIDDEN_KEY_PARTS = {
            "password", "secret", "token", "key", "credential", "cookie", "authorization", "command"};

    public AuditRecord {
        Objects.requireNonNull(actor, "actor");
        StateValues.requireText("action", action, 96);
        StateValues.requireText("target", target, 512);
        StateValues.requireText("outcome", outcome, 32);
        StateValues.optionalText("operationId", operationId, 64);
        metadata = metadata == null ? Map.of() : safeMetadata(metadata);
    }

    public static AuditRecord of(AuditActor actor, String action, String target, String outcome, String operationId) {
        return new AuditRecord(actor, action, target, outcome, operationId, Map.of());
    }

    private static Map<String, String> safeMetadata(Map<String, String> input) {
        if (input.size() > MAX_METADATA_ENTRIES) {
            throw new StateException("invalid_request", "too many audit metadata entries");
        }
        TreeMap<String, String> out = new TreeMap<>();
        for (Map.Entry<String, String> e : input.entrySet()) {
            String k = StateValues.requireText("audit metadata key", e.getKey(), 64);
            String lower = k.toLowerCase(Locale.ROOT);
            for (String forbidden : FORBIDDEN_KEY_PARTS) {
                if (lower.contains(forbidden)) {
                    throw new StateException("invalid_request", "audit metadata key not allowed: " + k);
                }
            }
            String v = e.getValue() == null ? "" : e.getValue();
            if (v.length() > MAX_METADATA_VALUE) {
                throw new StateException("invalid_request", "audit metadata value too long: " + k);
            }
            out.put(k, v);
        }
        return java.util.Collections.unmodifiableMap(out);
    }
}
