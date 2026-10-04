package com.vesta.wrapper.operations;

import java.time.Instant;
import java.util.List;

/** Snapshot of a durable operation. */
public record Operation(String id, long seq, String serverId, String actorPrincipalId, String deviceId,
                        String source, String type, String target, String payloadHash, String idempotencyKey,
                        OperationState state, boolean requiresConfirmation, int progressPercent,
                        String progressMessage, boolean cancelRequested, boolean externalEffectStarted,
                        boolean externalEffectIdempotent, String resultJson, String errorCode,
                        String errorMessage, Instant createdAt, Instant updatedAt, Instant finishedAt,
                        List<String> lockResources) {
}
