package com.vesta.wrapper.operations;

import com.google.gson.Strictness;
import com.google.gson.stream.JsonReader;
import com.google.gson.stream.JsonToken;
import com.vesta.wrapper.authorization.AuthorizationService;
import com.vesta.wrapper.authorization.Capability;
import com.vesta.wrapper.authorization.EffectivePolicy;
import com.vesta.wrapper.identity.IdentityRecords.RemoteActor;
import com.vesta.wrapper.state.EventLog;
import com.vesta.wrapper.state.StateDatabase;
import com.vesta.wrapper.state.StateException;

import java.io.IOException;
import java.io.StringReader;
import java.nio.charset.StandardCharsets;
import java.time.Instant;
import java.util.ArrayList;
import java.util.Base64;
import java.util.HashSet;
import java.util.List;
import java.util.Objects;
import java.util.Set;
import java.util.function.Supplier;
import java.util.regex.Pattern;

/**
 * Trusted in-process, bounded replay of sanitized operation lifecycle events from the durable journal.
 *
 * <p>Every call resolves the <em>current</em> exact {@code tasks.view} capability inside the same read
 * snapshot that validates the cursor, reads the journal bounds and selects events. {@link RemoteActor}
 * identifiers must come from a trusted authenticated transport; neither they nor a cursor prove identity.
 *
 * <p>Only the three operation event kinds written by {@link OperationService} are projected, through a
 * strict parser into typed fields. Nothing else in the journal (other kinds, raw JSON, free-form text) is
 * returned. There is no network route here; this is a contract for a future authenticated transport.
 *
 * <p>A cursor is an opaque, canonical, bounded token carrying the server id, the position of the last
 * processed journal row and the high-water mark captured when the replay window opened. It is neither a
 * credential nor a signature and it cannot prove the journal was not replaced by a different history with
 * the same server identity. Restore/replacement detection is not provided.
 */
public final class OperationEventReplayService {
    public static final int API_VERSION = 1;
    public static final int MAX_RETURN = 100;
    public static final int MAX_SCAN = 500;
    public static final int MAX_CURSOR_CHARS = 256;

    public static final String CREATED = "operation.created";
    public static final String STATE = "operation.state";
    public static final String PROGRESS = "operation.progress";
    private static final List<String> KINDS = List.of(CREATED, PROGRESS, STATE);
    private static final Set<String> KIND_SET = Set.copyOf(KINDS);

    private static final Pattern TOKEN = Pattern.compile("[A-Za-z0-9_-]{1,%d}".formatted(MAX_CURSOR_CHARS));
    private static final Pattern NUMBER = Pattern.compile("0|[1-9][0-9]{0,18}");
    private static final Pattern OPERATION_ID =
            Pattern.compile("[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}");
    private static final Pattern PROGRESS_VALUE = Pattern.compile("0|[1-9][0-9]?|100");

    public enum Status {
        /** Events (possibly none) were read from the requested position. */
        OK,
        /** The requested position precedes retained history: the client must resynchronize. */
        RESYNC_REQUIRED
    }

    /**
     * Sanitized operation event. {@code eventId} is the decimal journal position as a string (safe for
     * clients with limited integer precision); {@code type} is one of the three operation event kinds.
     */
    public record Event(String eventId, String type, String operationId, Instant at, OperationState state,
                        int progress) {
    }

    /**
     * One replay page. {@code nextCursor} resumes strictly after the last processed row inside the captured
     * window ({@code null} only for {@link Status#RESYNC_REQUIRED}); {@code complete} is true once the window
     * captured by {@code openReplay} has been fully read. {@code retentionFloor} is the decimal pruning
     * watermark.
     */
    public record Page(int apiVersion, Status status, List<Event> events, String nextCursor, boolean complete,
                       String retentionFloor) {
        public Page {
            events = List.copyOf(events);
        }
    }

    private record Cursor(long position, long highWater) {
    }

    private final StateDatabase db;
    private final EventLog events;
    private final AuthorizationService authorization;

    public OperationEventReplayService(StateDatabase db, EventLog events, AuthorizationService authorization) {
        this.db = Objects.requireNonNull(db, "db");
        this.events = Objects.requireNonNull(events, "events");
        this.authorization = Objects.requireNonNull(authorization, "authorization");
    }

    /**
     * Opens a replay window: captures the current journal high-water mark and reads the first page strictly
     * after {@code checkpoint} ({@code null} = from the beginning of history). A checkpoint is a cursor
     * returned by this service; its embedded high-water mark is ignored and a fresh one is captured.
     */
    public Page openReplay(RemoteActor actor, String checkpoint, int limit) {
        return guarded(() -> page(actor, checkpoint, true, limit));
    }

    /** Continues a window opened by {@link #openReplay}; newly appended events never extend it. */
    public Page readPage(RemoteActor actor, String continuation, int limit) {
        if (continuation == null) {
            throw new StateException("invalid_cursor", "Replay cursor is invalid");
        }
        return guarded(() -> page(actor, continuation, false, limit));
    }

    /**
     * Returns an empty, complete page whose cursor is the current journal head. A client that received
     * {@link Status#RESYNC_REQUIRED} obtains the head from the same transactional snapshot as its resynchronized
     * state through a future snapshot contract; this method alone does not make state and cursor consistent.
     */
    public Page currentCheckpoint(RemoteActor actor) {
        return guarded(() -> db.read(c -> {
            authorize(c, actor);
            EventLog.Bounds bounds = events.bounds(c);
            return new Page(API_VERSION, Status.OK, List.of(), encode(new Cursor(bounds.latest(), bounds.latest())),
                    true, Long.toString(bounds.prunedThrough()));
        }));
    }

    private Page page(RemoteActor actor, String token, boolean open, int limit) {
        if (limit < 1 || limit > MAX_RETURN) {
            throw new StateException("invalid_request", "Replay limit is out of range");
        }
        return db.read(c -> {
            authorize(c, actor);
            EventLog.Bounds bounds = events.bounds(c);
            Cursor in = token == null ? new Cursor(0, bounds.latest()) : decode(token);
            long position = in.position();
            long high = open ? bounds.latest() : in.highWater();
            if (position > high || high > bounds.latest()) {
                throw invalidCursor();
            }
            String floor = Long.toString(bounds.prunedThrough());
            if (position < bounds.prunedThrough()) {
                return new Page(API_VERSION, Status.RESYNC_REQUIRED, List.of(), null, false, floor);
            }
            if (position == high) {
                return new Page(API_VERSION, Status.OK, List.of(), encode(new Cursor(position, high)), true, floor);
            }
            List<EventLog.ScanRow> rows = events.scan(c, position, high, MAX_SCAN, KINDS, EventLog.MAX_PAYLOAD_CHARS);
            List<Event> out = new ArrayList<>();
            long processed = position;
            boolean stopped = false;
            for (EventLog.ScanRow row : rows) {
                if (KIND_SET.contains(row.type())) {
                    if (out.size() == limit) {
                        stopped = true;
                        break;
                    }
                    out.add(project(row));
                }
                processed = row.cursor();
            }
            if (!stopped && rows.size() < MAX_SCAN) {
                processed = high; // no further rows exist before the horizon (holes from rolled-back inserts)
            }
            boolean complete = processed >= high;
            return new Page(API_VERSION, Status.OK, out, encode(new Cursor(processed, high)), complete, floor);
        });
    }

    private void authorize(java.sql.Connection c, RemoteActor actor) throws java.sql.SQLException {
        if (actor == null) {
            throw new StateException("forbidden", "Not permitted");
        }
        EffectivePolicy policy = authorization.resolve(c, actor.principalId(), actor.deviceId());
        if (!policy.check(Capability.TASKS_VIEW, "").allowed()) {
            throw new StateException("forbidden", "Not permitted");
        }
    }

    private static Event project(EventLog.ScanRow row) {
        if (row.payloadJson() == null || row.operationId() == null || !OPERATION_ID.matcher(row.operationId()).matches()) {
            throw malformed();
        }
        String id = null;
        String state = null;
        String progress = null;
        try (JsonReader reader = new JsonReader(new StringReader(row.payloadJson()))) {
            reader.setStrictness(Strictness.STRICT);
            reader.beginObject();
            Set<String> seen = new HashSet<>();
            while (reader.hasNext()) {
                String name = reader.nextName();
                if (!seen.add(name)) {
                    throw malformed();
                }
                switch (name) {
                    case "operationId" -> id = text(reader);
                    case "state" -> state = text(reader);
                    case "progress" -> {
                        if (reader.peek() != JsonToken.NUMBER) {
                            throw malformed();
                        }
                        progress = reader.nextString();
                    }
                    default -> throw malformed();
                }
            }
            reader.endObject();
            if (reader.peek() != JsonToken.END_DOCUMENT) {
                throw malformed();
            }
        } catch (IOException | IllegalStateException | IllegalArgumentException e) {
            throw malformed();
        }
        if (id == null || state == null || progress == null || !id.equals(row.operationId())
                || !PROGRESS_VALUE.matcher(progress).matches()) {
            throw malformed();
        }
        try {
            return new Event(Long.toString(row.cursor()), row.type(), id, Instant.ofEpochMilli(row.atMillis()),
                    OperationState.fromWire(state), Integer.parseInt(progress));
        } catch (IllegalArgumentException | java.time.DateTimeException e) {
            throw malformed();
        }
    }

    private static String text(JsonReader reader) throws IOException {
        if (reader.peek() != JsonToken.STRING) {
            throw malformed();
        }
        return reader.nextString();
    }

    private String encode(Cursor cursor) {
        String plain = "v1." + db.serverId() + "." + cursor.position() + "." + cursor.highWater();
        return Base64.getUrlEncoder().withoutPadding().encodeToString(plain.getBytes(StandardCharsets.US_ASCII));
    }

    private Cursor decode(String token) {
        if (!TOKEN.matcher(token).matches()) {
            throw invalidCursor();
        }
        try {
            byte[] raw = Base64.getUrlDecoder().decode(token);
            if (!Base64.getUrlEncoder().withoutPadding().encodeToString(raw).equals(token)) {
                throw invalidCursor(); // non-canonical encoding
            }
            String plain = new String(raw, StandardCharsets.US_ASCII);
            String[] parts = plain.split("\\.", -1);
            if (parts.length != 4 || !"v1".equals(parts[0]) || !db.serverId().equals(parts[1])
                    || !NUMBER.matcher(parts[2]).matches() || !NUMBER.matcher(parts[3]).matches()) {
                throw invalidCursor();
            }
            return new Cursor(Long.parseLong(parts[2]), Long.parseLong(parts[3]));
        } catch (IllegalArgumentException e) {
            throw invalidCursor();
        }
    }

    /** Fixed codes and messages only: no SQL text, stored payload or underlying cause is exported. */
    private static <T> T guarded(Supplier<T> work) {
        try {
            return work.get();
        } catch (StateException e) {
            throw new StateException(e.getCode(), "Operation event replay failed: " + e.getCode());
        } catch (RuntimeException e) {
            throw new StateException("replay_unavailable", "Operation event replay failed: replay_unavailable");
        }
    }

    private static StateException invalidCursor() {
        return new StateException("invalid_cursor", "Replay cursor is invalid");
    }

    private static StateException malformed() {
        return new StateException("event_malformed", "Operation event is malformed");
    }
}
