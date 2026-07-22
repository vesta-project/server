package com.vesta.wrapper.runtime;

import org.junit.jupiter.api.Test;

import java.util.Optional;

import static org.junit.jupiter.api.Assertions.*;

class PlayerListParserTest {

    @Test
    void parsesVanillaEmptyList() {
        Optional<PlayerListParser.Result> parsed = PlayerListParser.parseLine(
                "There are 0 of a max of 20 players online:");
        assertTrue(parsed.isPresent());
        assertEquals(0, parsed.get().online);
        assertEquals(20, parsed.get().max);
        assertTrue(parsed.get().players.isEmpty());
    }

    @Test
    void parsesVanillaNamedPlayers() {
        Optional<PlayerListParser.Result> parsed = PlayerListParser.parseLine(
                "There are 2 of a max of 20 players online: Steve, Alex");
        assertTrue(parsed.isPresent());
        assertEquals(2, parsed.get().online);
        assertEquals(20, parsed.get().max);
        assertEquals(2, parsed.get().players.size());
        assertEquals("Steve", parsed.get().players.get(0).name);
        assertNull(parsed.get().players.get(0).uuid);
        assertEquals("Alex", parsed.get().players.get(1).name);
    }

    @Test
    void parsesSlashFormAndLogPrefix() {
        Optional<PlayerListParser.Result> parsed = PlayerListParser.parseLine(
                "[12:00:01] [Server thread/INFO]: There are 1/20 players online: Notch");
        assertTrue(parsed.isPresent());
        assertEquals(1, parsed.get().online);
        assertEquals(20, parsed.get().max);
        assertEquals("Notch", parsed.get().players.get(0).name);
    }

    @Test
    void parsesOptionalUuidSuffix() {
        Optional<PlayerListParser.Result> parsed = PlayerListParser.parseLine(
                "There are 1 of a max of 8 players online: Steve (00000000-0000-0000-0000-000000000001)");
        assertTrue(parsed.isPresent());
        assertEquals("Steve", parsed.get().players.get(0).name);
        assertEquals("00000000-0000-0000-0000-000000000001", parsed.get().players.get(0).uuid);
    }

    @Test
    void rejectsNonListLines() {
        assertTrue(PlayerListParser.parseLine("Done (3.2s)! For help, type \"help\"").isEmpty());
        assertTrue(PlayerListParser.parseLine("").isEmpty());
        assertTrue(PlayerListParser.parseLine(null).isEmpty());
    }
}
