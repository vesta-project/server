package com.vesta.wrapper.console;

import org.junit.jupiter.api.Test;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertTrue;

class ConsoleBufferTest {

    @Test
    void snapshotReturnsLinesAfterCursor() {
        ConsoleBuffer buffer = new ConsoleBuffer();
        buffer.append("stdout", "first");
        buffer.append("stdout", "second");

        ConsoleBuffer.Snapshot snapshot = buffer.snapshot(0, 10);
        assertEquals(2, snapshot.lines.size());
        assertEquals("first", snapshot.lines.get(0).text);
        assertEquals("second", snapshot.lines.get(1).text);
        assertEquals(2, snapshot.cursor);

        ConsoleBuffer.Snapshot partial = buffer.snapshot(1, 10);
        assertEquals(1, partial.lines.size());
        assertEquals("second", partial.lines.get(0).text);
    }

    @Test
    void evictsOldestLinesWhenLineCapExceeded() {
        ConsoleBuffer buffer = new ConsoleBuffer();
        for (int i = 0; i < 10_050; i++) {
            buffer.append("stdout", "line-" + i);
        }
        ConsoleBuffer.Snapshot snapshot = buffer.snapshot(0, 20_000);
        assertTrue(snapshot.lines.size() <= 10_000);
        assertTrue(snapshot.lines.get(0).text.startsWith("line-"));
        assertTrue(Integer.parseInt(snapshot.lines.get(0).text.substring(5)) >= 50);
    }
}
