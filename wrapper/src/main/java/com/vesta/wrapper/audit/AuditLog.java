package com.vesta.wrapper.audit;

import com.google.gson.Gson;
import com.google.gson.JsonArray;
import com.vesta.wrapper.state.StateDatabase;
import com.vesta.wrapper.state.StateException;
import com.vesta.wrapper.state.StateValues;

import java.sql.Connection;
import java.sql.PreparedStatement;
import java.sql.ResultSet;
import java.sql.SQLException;
import java.time.Instant;
import java.util.ArrayList;
import java.util.List;

/**
 * Protected, append-only, server-scoped audit log with a SHA-256 hash chain.
 *
 * <p>Append happens inside the caller's write transaction, so an audit failure aborts the
 * audited mutation (fail closed). UPDATE/DELETE are blocked by schema triggers. The chain is
 * tamper-evident against edits of existing rows; it is not tamper-proof against an attacker
 * with write access to the database who rewrites the whole chain or truncates the tail —
 * external checkpoints are future work.
 */
public final class AuditLog {
    public static final String GENESIS_HASH = "0".repeat(64);
    public static final int MAX_PAGE = 500;
    private static final Gson GSON = new Gson();

    private final StateDatabase db;

    /**
     * Opens the audit service and verifies the full persisted chain (O(n) on startup).
     *
     * @throws StateException {@code audit_chain_corrupt} if any persisted entry fails verification;
     *         callers must refuse to run audited mutations in that case. Local deletion of the newest
     *         entries (tail truncation) is NOT detectable by this check.
     */
    public AuditLog(StateDatabase db) {
        this.db = db;
        AuditVerification v = verifyChain();
        if (!v.valid()) {
            throw new StateException("audit_chain_corrupt",
                    "Audit hash chain verification failed at seq " + v.firstBrokenSeq());
        }
    }

    /** Appends a record inside the caller's write transaction; returns its sequence number. */
    public long append(Connection c, AuditRecord record) throws SQLException {
        String prev = GENESIS_HASH;
        try (PreparedStatement ps = c.prepareStatement("SELECT hash FROM audit_events ORDER BY seq DESC LIMIT 1");
             ResultSet rs = ps.executeQuery()) {
            if (rs.next()) {
                prev = rs.getString(1);
            }
        }
        long at = db.clock().millis();
        String metadataJson = GSON.toJson(record.metadata());
        AuditActor a = record.actor();
        String hash = chainHash(prev, db.serverId(), a.principalId(), a.deviceId(), a.source(), record.action(),
                record.target(), record.outcome(), record.operationId(), at, metadataJson);
        try (PreparedStatement ps = c.prepareStatement(
                "INSERT INTO audit_events (server_id, principal_id, device_id, source, action, target, outcome, "
                        + "operation_id, at, metadata_json, prev_hash, hash) VALUES (?,?,?,?,?,?,?,?,?,?,?,?)")) {
            ps.setString(1, db.serverId());
            ps.setString(2, a.principalId());
            ps.setString(3, a.deviceId());
            ps.setString(4, a.source());
            ps.setString(5, record.action());
            ps.setString(6, record.target());
            ps.setString(7, record.outcome());
            ps.setString(8, record.operationId());
            ps.setLong(9, at);
            ps.setString(10, metadataJson);
            ps.setString(11, prev);
            ps.setString(12, hash);
            ps.executeUpdate();
        }
        try (PreparedStatement ps = c.prepareStatement("SELECT last_insert_rowid()");
             ResultSet rs = ps.executeQuery()) {
            rs.next();
            return rs.getLong(1);
        }
    }

    /** Appends a record in its own transaction (for audit-only events such as rejected requests). */
    public long append(AuditRecord record) {
        return db.write(c -> append(c, record));
    }

    /** Recomputes the whole chain. */
    public AuditVerification verifyChain() {
        return db.read(c -> {
            String expectedPrev = GENESIS_HASH;
            long checked = 0;
            try (PreparedStatement ps = c.prepareStatement(
                    "SELECT seq, server_id, principal_id, device_id, source, action, target, outcome, operation_id, "
                            + "at, metadata_json, prev_hash, hash FROM audit_events ORDER BY seq");
                 ResultSet rs = ps.executeQuery()) {
                while (rs.next()) {
                    AuditEntry e = map(rs);
                    String recomputed = chainHash(e.prevHash(), e.serverId(), e.principalId(), e.deviceId(),
                            e.source(), e.action(), e.target(), e.outcome(), e.operationId(),
                            e.at().toEpochMilli(), e.metadataJson());
                    if (!e.prevHash().equals(expectedPrev) || !recomputed.equals(e.hash())
                            || !db.serverId().equals(e.serverId())) {
                        return new AuditVerification(false, checked, e.seq());
                    }
                    expectedPrev = e.hash();
                    checked++;
                }
            }
            return new AuditVerification(true, checked, -1);
        });
    }

    /** Reads entries with seq greater than {@code afterSeq}, bounded page size. */
    public List<AuditEntry> page(long afterSeq, int limit) {
        int bounded = Math.max(1, Math.min(MAX_PAGE, limit));
        return db.read(c -> {
            List<AuditEntry> out = new ArrayList<>();
            try (PreparedStatement ps = c.prepareStatement(
                    "SELECT seq, server_id, principal_id, device_id, source, action, target, outcome, operation_id, "
                            + "at, metadata_json, prev_hash, hash FROM audit_events WHERE seq > ? ORDER BY seq LIMIT ?")) {
                ps.setLong(1, afterSeq);
                ps.setInt(2, bounded);
                try (ResultSet rs = ps.executeQuery()) {
                    while (rs.next()) {
                        out.add(map(rs));
                    }
                }
            }
            return List.copyOf(out);
        });
    }

    private static AuditEntry map(ResultSet rs) throws SQLException {
        return new AuditEntry(rs.getLong(1), rs.getString(2), rs.getString(3), rs.getString(4), rs.getString(5),
                rs.getString(6), rs.getString(7), rs.getString(8), rs.getString(9),
                Instant.ofEpochMilli(rs.getLong(10)), rs.getString(11), rs.getString(12), rs.getString(13));
    }

    /** Canonical, unambiguous encoding (JSON array of fields) hashed with SHA-256. */
    static String chainHash(String prevHash, String serverId, String principalId, String deviceId, String source,
                            String action, String target, String outcome, String operationId, long at,
                            String metadataJson) {
        JsonArray fields = new JsonArray();
        fields.add("vesta-audit-v1");
        fields.add(prevHash);
        fields.add(serverId);
        fields.add(principalId);
        fields.add(deviceId);
        fields.add(source);
        fields.add(action);
        fields.add(target);
        fields.add(outcome);
        fields.add(operationId);
        fields.add(at);
        fields.add(metadataJson);
        return StateValues.sha256Hex(GSON.toJson(fields));
    }
}
