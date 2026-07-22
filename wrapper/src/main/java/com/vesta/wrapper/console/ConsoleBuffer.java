package com.vesta.wrapper.console;

import java.time.Instant;
import java.util.ArrayList;
import java.util.List;
import java.util.concurrent.CopyOnWriteArrayList;
import java.util.function.Consumer;

/**
 * Thread-safe ring buffer of Minecraft server console lines.
 */
public final class ConsoleBuffer {

    public static final class Line {
        public final long cursor;
        public final Instant ts;
        public final String stream;
        public final String text;

        public Line(long cursor, Instant ts, String stream, String text) {
            this.cursor = cursor;
            this.ts = ts;
            this.stream = stream;
            this.text = text;
        }
    }

    public static final class Snapshot {
        public final long cursor;
        public final List<Line> lines;

        public Snapshot(long cursor, List<Line> lines) {
            this.cursor = cursor;
            this.lines = lines;
        }
    }

    private static final int MAX_LINES = 10_000;
    private static final int MAX_BYTES = 2 * 1024 * 1024;

    private final Object lock = new Object();
    private final List<Line> lines = new ArrayList<>();
    private final CopyOnWriteArrayList<Consumer<Line>> listeners = new CopyOnWriteArrayList<>();
    private long nextCursor = 1;
    private int totalBytes;

    public void append(String stream, String text) {
        if (text == null || text.isEmpty()) {
            return;
        }
        String normalizedStream = stream != null ? stream : "stdout";
        Line line;
        List<Consumer<Line>> toNotify;
        synchronized (lock) {
            evictIfNeeded(text.length());
            line = new Line(nextCursor++, Instant.now(), normalizedStream, text);
            lines.add(line);
            totalBytes += text.length();
            toNotify = List.copyOf(listeners);
        }
        for (Consumer<Line> listener : toNotify) {
            try {
                listener.accept(line);
            } catch (RuntimeException ignored) {
                // listener failures must not break capture
            }
        }
    }

    public Snapshot snapshot(long sinceCursor, int limit) {
        int max = limit > 0 ? Math.min(limit, MAX_LINES) : MAX_LINES;
        synchronized (lock) {
            List<Line> out = new ArrayList<>();
            for (Line line : lines) {
                if (line.cursor > sinceCursor) {
                    out.add(line);
                    if (out.size() >= max) {
                        break;
                    }
                }
            }
            long cursor = lines.isEmpty() ? 0 : lines.get(lines.size() - 1).cursor;
            return new Snapshot(cursor, List.copyOf(out));
        }
    }

    public long latestCursor() {
        synchronized (lock) {
            return lines.isEmpty() ? 0 : lines.get(lines.size() - 1).cursor;
        }
    }

    public void addListener(Consumer<Line> listener) {
        if (listener != null) {
            listeners.addIfAbsent(listener);
        }
    }

    public void removeListener(Consumer<Line> listener) {
        listeners.remove(listener);
    }

    private void evictIfNeeded(int incomingBytes) {
        while ((!lines.isEmpty() && lines.size() >= MAX_LINES)
                || (totalBytes + incomingBytes > MAX_BYTES && !lines.isEmpty())) {
            Line removed = lines.remove(0);
            totalBytes -= removed.text.length();
        }
    }
}
