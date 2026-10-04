package com.vesta.wrapper.audit;

/**
 * Result of verifying the audit hash chain.
 *
 * @param firstBrokenSeq sequence of the first entry that fails verification, or -1 when valid
 */
public record AuditVerification(boolean valid, long checkedEntries, long firstBrokenSeq) {
}
