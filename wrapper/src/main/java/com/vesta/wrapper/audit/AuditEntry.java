package com.vesta.wrapper.audit;

import java.time.Instant;

/** A persisted audit event in the server's hash chain. */
public record AuditEntry(long seq, String serverId, String principalId, String deviceId, String source,
                         String action, String target, String outcome, String operationId, Instant at,
                         String metadataJson, String prevHash, String hash) {
}
