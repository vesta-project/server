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

    private static long prunedThrough(Connection c) throws SQLException {
        try (PreparedStatement ps = c.prepareStatement("SELECT value FROM meta WHERE key = 'events_pruned_through'");
             ResultSet rs = ps.executeQuery()) {
            return rs.next() ? Long.parseLong(rs.getString(1)) : 0L;
        }
    }
}
