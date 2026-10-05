package com.vesta.wrapper.state;

import org.junit.jupiter.api.AfterEach;
import org.junit.jupiter.api.BeforeEach;
import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.io.TempDir;

import java.nio.file.Path;
import java.time.Clock;
import java.time.Duration;
import java.time.Instant;
import java.time.ZoneOffset;
import java.util.ArrayList;
import java.util.List;

import static org.junit.jupiter.api.Assertions.*;

class EventLogBoundedScanTest {
    @TempDir Path root;
    private final Clock clock = Clock.fixed(Instant.parse("2026-10-05T12:00:00Z"), ZoneOffset.UTC);
    private StateDatabase db;
    private EventLog events;

    @BeforeEach void open() {
        db = StateDatabase.open(new StateDirectory(root.resolve("state")), clock);
        events = new EventLog(db);
    }

    @AfterEach void close() { db.close(); }

    private void append(String type, String operationId, String payload) {
        db.write(c -> events.append(c, type, operationId, payload));
    }

    private void rawAppend(String type, String payload) {
        db.write(c -> { try (var ps = c.prepareStatement(
                "INSERT INTO events (server_id, type, operation_id, at, payload_json) VALUES (?,?,NULL,0,?)")) {
            ps.setString(1, db.serverId()); ps.setString(2, type); ps.setString(3, payload); ps.executeUpdate();
        } return null; });
    }

    @Test void boundsDescribeEmptyRetainedAndFullyPrunedJournalsInOneSnapshot() {
        assertEquals(new EventLog.Bounds(0, 0, 0), db.read(events::bounds));
        append("a.kind", null, "{}"); append("a.kind", null, "{}"); append("a.kind", null, "{}");
        assertEquals(new EventLog.Bounds(0, 1, 3), db.read(events::bounds));
        assertEquals(3, events.pruneOlderThan(clock.instant().plus(Duration.ofSeconds(1))));
        assertEquals(new EventLog.Bounds(3, 0, 3), db.read(events::bounds));
        append("a.kind", null, "{}");
        assertEquals(new EventLog.Bounds(3, 4, 4), db.read(events::bounds));
    }

    @Test void scanProjectsOnlyRequestedTypesWithinThePayloadLimitAndTruncatesHeaders() {
        append("wanted.kind", "operation-1", "{}");
        append("other.kind", "operation-2", "{\"secret\":\"never-projected\"}");
        rawAppend("wanted.kind", "x".repeat(5_000));
        rawAppend("y".repeat(100), "{}");
        List<EventLog.ScanRow> rows = db.read(c -> events.scan(c, 0, 100, 10, List.of("wanted.kind"), 4));
        assertEquals(4, rows.size());
        assertEquals("{}", rows.get(0).payloadJson()); assertEquals(2, rows.get(0).payloadChars());
        assertEquals("operation-1", rows.get(0).operationId());
        assertNull(rows.get(1).payloadJson()); assertEquals(0, rows.get(1).payloadChars());
        assertNull(rows.get(2).payloadJson()); assertEquals(5_000, rows.get(2).payloadChars());
        assertEquals(65, rows.get(3).type().length()); assertNull(rows.get(3).payloadJson());
        assertFalse(rows.toString().contains("never-projected"));
    }

    @Test void scanRangeIsExclusiveAfterInclusiveThroughAndLimited() {
        for (int i = 0; i < 6; i++) append("a.kind", null, "{}");
        assertEquals(List.of(3L, 4L), cursors(db.read(c -> events.scan(c, 2, 4, 10, List.of("a.kind"), 100))));
        assertEquals(List.of(1L, 2L), cursors(db.read(c -> events.scan(c, 0, 6, 2, List.of("a.kind"), 100))));
        assertTrue(db.read(c -> events.scan(c, 6, 6, 10, List.of("a.kind"), 100)).isEmpty());
        assertTrue(db.read(c -> events.scan(c, 5, 3, 10, List.of("a.kind"), 100)).isEmpty());
    }

    @Test void invalidScanRequestsAreRejectedBeforeAnyQuery() {
        List<String> nine = new ArrayList<>(); for (int i = 0; i < 9; i++) nine.add("t" + i);
        for (var request : List.<java.util.function.Supplier<Object>>of(
                () -> db.read(c -> events.scan(c, 0, 1, 0, List.of("a"), 10)),
                () -> db.read(c -> events.scan(c, 0, 1, EventLog.MAX_PAGE + 1, List.of("a"), 10)),
                () -> db.read(c -> events.scan(c, 0, 1, 1, List.of(), 10)),
                () -> db.read(c -> events.scan(c, 0, 1, 1, nine, 10)))) {
            assertEquals("invalid_request", assertThrows(StateException.class, request::get).getCode());
        }
    }

    @Test void existingTrustedReadStillBehavesIdentically() {
        append("a.kind", null, "{}"); append("a.kind", null, "{}");
        var page = events.read(0, 1);
        assertEquals(1, page.events().size()); assertEquals(1, page.nextCursor()); assertFalse(page.gap());
        assertEquals(1, page.oldestAvailableCursor());
    }

    private static List<Long> cursors(List<EventLog.ScanRow> rows) { return rows.stream().map(EventLog.ScanRow::cursor).toList(); }
}
