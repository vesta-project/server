package com.vesta.wrapper.state;

import java.sql.Connection;
import java.sql.PreparedStatement;
import java.sql.ResultSet;
import java.sql.SQLException;
import java.time.Instant;
import java.util.ArrayList;
import java.util.List;

/**
 * Durable, server-scoped event stream with resumable cursors, bounded pagination and
 * explicit retention gaps. Payloads must be small, non-secret JSON.
 */
public final class EventLog {
    public static final int MAX_PAGE = 500;
    public static final int MAX_PAYLOAD_CHARS = 4_096;

    public record StoredEvent(long cursor, String serverId, String type, String operationId,
                              Instant at, String payloadJson) {
    }

    /**
     * @param gap true when events after the requested cursor were pruned; clients must resync
     * @param oldestAvailableCursor lowest cursor still retained, or 0 when none retained
     */
    public record EventPage(List<StoredEvent> events, long nextCursor, boolean gap, long oldestAvailableCursor) {
    }

    private final StateDatabase db;

    public EventLog(StateDatabase db) {
        this.db = db;
    }

    /** Appends inside the caller's write transaction. */
    public long append(Connection c, String type, String operationId, String payloadJson) throws SQLException {
        StateValues.requireText("event type", type, 64);
        String payload = payloadJson == null ? "{}" : payloadJson;
        if (payload.length() > MAX_PAYLOAD_CHARS) {
            throw new StateException("invalid_request", "event payload too large");
        }
        try (PreparedStatement ps = c.prepareStatement(
                "INSERT INTO events (server_id, type, operation_id, at, payload_json) VALUES (?,?,?,?,?)")) {
            ps.setString(1, db.serverId());
            ps.setString(2, type);
            ps.setString(3, operationId);
            ps.setLong(4, db.clock().millis());
            ps.setString(5, payload);
            ps.executeUpdate();
        }
        try (PreparedStatement ps = c.prepareStatement("SELECT last_insert_rowid()");
             ResultSet rs = ps.executeQuery()) {
            rs.next();
            return rs.getLong(1);
        }
    }

    /** Reads events strictly after {@code afterCursor} (0 = from the beginning). */
    public EventPage read(long afterCursor, int limit) {
        int bounded = Math.max(1, Math.min(MAX_PAGE, limit));
        return db.read(c -> {
            long prunedThrough = prunedThrough(c);
            long oldest = 0;
            try (PreparedStatement ps = c.prepareStatement("SELECT MIN(cursor) FROM events");
                 ResultSet rs = ps.executeQuery()) {
                if (rs.next()) {
                    oldest = rs.getLong(1);
                }
            }
            boolean gap = afterCursor < prunedThrough;
            List<StoredEvent> events = new ArrayList<>();
            long next = Math.max(afterCursor, gap ? prunedThrough : afterCursor);
            try (PreparedStatement ps = c.prepareStatement(
                    "SELECT cursor, server_id, type, operation_id, at, payload_json FROM events "
                            + "WHERE cursor > ? ORDER BY cursor LIMIT ?")) {
                ps.setLong(1, afterCursor);
                ps.setInt(2, bounded);
                try (ResultSet rs = ps.executeQuery()) {
                    while (rs.next()) {
                        StoredEvent e = new StoredEvent(rs.getLong(1), rs.getString(2), rs.getString(3),
                                rs.getString(4), Instant.ofEpochMilli(rs.getLong(5)), rs.getString(6));
                        events.add(e);
                        next = e.cursor();
                    }
                }
            }
            return new EventPage(List.copyOf(events), next, gap, oldest);
        });
    }

    /**
     * Prefix eviction: deletes the longest cursor-ordered prefix of events whose timestamps are all
     * older than {@code cutoff}, stopping at the first retained (non-expired) event. Events are never
     * removed from the middle of the stream, so after a clock rollback an expired event with a higher
     * cursor than a non-expired one is retained until the prefix before it expires. The retention
     * watermark ({@code gap} in {@link #read}) therefore always describes a contiguous pruned prefix.
     */
    public int pruneOlderThan(Instant cutoff) {
        return db.write(c -> {
            long maxPruned;
            try (PreparedStatement ps = c.prepareStatement(
                    "SELECT COALESCE((SELECT MIN(cursor) - 1 FROM events WHERE at >= ?), "
                            + "(SELECT COALESCE(MAX(cursor), 0) FROM events))")) {
                ps.setLong(1, cutoff.toEpochMilli());
                try (ResultSet rs = ps.executeQuery()) {
                    rs.next();
                    maxPruned = rs.getLong(1);
                }
            }
            if (maxPruned <= 0) {
                return 0;
            }
            int deleted; // may be 0 if this prefix was already pruned; watermark update is monotonic
            try (PreparedStatement ps = c.prepareStatement("DELETE FROM events WHERE cursor <= ?")) {
                ps.setLong(1, maxPruned);
                deleted = ps.executeUpdate();
            }
            try (PreparedStatement ps = c.prepareStatement(
                    "UPDATE meta SET value = ? WHERE key = 'events_pruned_through' AND CAST(value AS INTEGER) < ?")) {
                ps.setString(1, Long.toString(maxPruned));
                ps.setLong(2, maxPruned);
                ps.executeUpdate();
            }
            return deleted;
        });
    }

    /**
     * Journal boundaries observed in one snapshot.
     *
     * @param prunedThrough persisted retention watermark (every cursor at or below it was removed)
     * @param oldestRetained lowest retained cursor, or 0 when none is retained
     * @param latest highest cursor ever issued as far as this journal can tell: the retained maximum, or the
     *               retention watermark when every row was pruned
     */
    public record Bounds(long prunedThrough, long oldestRetained, long latest) {
    }

    /**
     * One journal row for a bounded scan. {@code payloadJson} is non-null only for a requested type whose
     * payload is within the character limit; {@code payloadChars} reports the stored length of a requested
     * type so an oversize payload can be distinguished from an absent one without being read into memory.
     */
    public record ScanRow(long cursor, String type, String operationId, long atMillis, String payloadJson,
                          int payloadChars) {
    }

    /** Reads the journal boundaries inside the caller's transaction. */
    public Bounds bounds(Connection c) throws SQLException {
        long pruned = prunedThrough(c);
        long oldest = 0;
        long max = 0;
        try (PreparedStatement ps = c.prepareStatement("SELECT MIN(cursor), MAX(cursor) FROM events");
             ResultSet rs = ps.executeQuery()) {
            if (rs.next()) {
                oldest = rs.getLong(1);
                max = rs.getLong(2);
            }
        }
        return new Bounds(pruned, oldest, Math.max(max, pruned));
    }

    /**
     * Bounded scan of cursors in {@code (after, through]} inside the caller's transaction. Header fields are
     * truncated in SQL and payloads are projected only for {@code payloadTypes} within
     * {@code maxPayloadChars}, so unrelated or oversize stored text is never loaded.
     */
    public List<ScanRow> scan(Connection c, long after, long through, int limit, List<String> payloadTypes,
                              int maxPayloadChars) throws SQLException {
        if (limit < 1 || limit > MAX_PAGE || payloadTypes.isEmpty() || payloadTypes.size() > 8) {
            throw new StateException("invalid_request", "invalid scan request");
        }
        String marks = String.join(",", java.util.Collections.nCopies(payloadTypes.size(), "?"));
        String sql = "SELECT cursor, substr(type, 1, 65), substr(operation_id, 1, 65), at, "
                + "CASE WHEN type IN (" + marks + ") AND length(payload_json) <= ? THEN payload_json END, "
                + "CASE WHEN type IN (" + marks + ") THEN length(payload_json) ELSE 0 END "
                + "FROM events WHERE cursor > ? AND cursor <= ? ORDER BY cursor LIMIT ?";
        List<ScanRow> rows = new ArrayList<>();
        try (PreparedStatement ps = c.prepareStatement(sql)) {
            int i = 1;
            for (String type : payloadTypes) {
                ps.setString(i++, type);
            }
            ps.setInt(i++, maxPayloadChars);
            for (String type : payloadTypes) {
                ps.setString(i++, type);
            }
            ps.setLong(i++, after);
            ps.setLong(i++, through);
            ps.setInt(i, limit);
            try (ResultSet rs = ps.executeQuery()) {
                while (rs.next()) {
                    rows.add(new ScanRow(rs.getLong(1), rs.getString(2), rs.getString(3), rs.getLong(4),
                            rs.getString(5), rs.getInt(6)));
                }
            }
        }
        return List.copyOf(rows);
    }

    private static long prunedThrough(Connection c) throws SQLException {
        try (PreparedStatement ps = c.prepareStatement("SELECT value FROM meta WHERE key = 'events_pruned_through'");
             ResultSet rs = ps.executeQuery()) {
            return rs.next() ? Long.parseLong(rs.getString(1)) : 0L;
        }
    }
}
