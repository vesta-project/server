package com.vesta.wrapper.operations;

import com.vesta.wrapper.audit.AuditActor;
import com.vesta.wrapper.state.StateValues;

import java.util.List;
import java.util.Objects;
import java.util.TreeSet;

/**
 * A request to create a durable operation.
 *
 * @param canonicalPayload canonical typed request input (bounded to {@link #MAX_PAYLOAD_CHARS}); it is
 *                         persisted for the registered handler and bound by its SHA-256. It must contain
 *                         only the minimum needed to execute: never secrets, credentials or raw commands
 *                         that are not the operation's declared input. Purged by retention after finish.
 * @param idempotencyKey optional client key, unique per server + actor
 * @param lockResources concrete resources (e.g. "game-generation", "files:/world") locked all-or-none
 * @param externalEffectIdempotent whether replaying the external effect after a crash is safe
 * @param requiresConfirmation destructive: starts in awaiting_confirmation and needs an exact confirmation
 */
public record OperationRequest(AuditActor actor, String type, String target, String canonicalPayload,
                               String idempotencyKey, List<String> lockResources,
                               boolean externalEffectIdempotent, boolean requiresConfirmation) {
    public static final int MAX_RESOURCES = 32;
    public static final int MAX_PAYLOAD_CHARS = 65_536;

    public OperationRequest {
        Objects.requireNonNull(actor, "actor");
        StateValues.requireText("type", type, 64);
        StateValues.requireText("target", target, 512);
        Objects.requireNonNull(canonicalPayload, "canonicalPayload");
        if (canonicalPayload.length() > MAX_PAYLOAD_CHARS) {
            throw new OperationException(OperationException.INVALID_REQUEST, "canonicalPayload too large");
        }
        StateValues.optionalText("idempotencyKey", idempotencyKey, 128);
        List<String> resources = lockResources == null ? List.of() : lockResources;
        if (resources.size() > MAX_RESOURCES) {
            throw new OperationException(OperationException.INVALID_REQUEST, "too many lock resources");
        }
        TreeSet<String> sorted = new TreeSet<>();
        for (String r : resources) {
            sorted.add(StateValues.requireText("lock resource", r, 256));
        }
        // Deterministic acquisition order, duplicates removed.
        lockResources = List.copyOf(sorted);
    }

    public String payloadHash() {
        return StateValues.sha256Hex(canonicalPayload);
    }
}
