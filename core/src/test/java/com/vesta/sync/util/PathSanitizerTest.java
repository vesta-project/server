package com.vesta.sync.util;

import org.junit.jupiter.api.Test;

import static org.junit.jupiter.api.Assertions.*;

class PathSanitizerTest {

    @Test
    void sanitizeNormalPath() {
        assertEquals("mods/sodium.jar", PathSanitizer.sanitize("mods/sodium.jar"));
    }

    @Test
    void sanitizeBackslashes() {
        assertEquals("mods/sodium.jar", PathSanitizer.sanitize("mods\\sodium.jar"));
    }

    @Test
    void sanitizeRejectsAbsolute() {
        assertThrows(IllegalArgumentException.class, () -> PathSanitizer.sanitize("/etc/passwd"));
    }

    @Test
    void sanitizeRejectsTraversal() {
        assertThrows(IllegalArgumentException.class, () -> PathSanitizer.sanitize("../mods/sodium.jar"));
    }

    @Test
    void sanitizeRejectsMidTraversal() {
        assertThrows(IllegalArgumentException.class, () -> PathSanitizer.sanitize("mods/../../config"));
    }

    @Test
    void sanitizeRejectsEmpty() {
        assertThrows(IllegalArgumentException.class, () -> PathSanitizer.sanitize(""));
    }

    @Test
    void sanitizeRejectsNull() {
        assertThrows(IllegalArgumentException.class, () -> PathSanitizer.sanitize(null));
    }

    @Test
    void sanitizeRejectsColon() {
        assertThrows(IllegalArgumentException.class, () -> PathSanitizer.sanitize("C:/mods/sodium.jar"));
    }

    @Test
    void hashPath() {
        String hash = "abcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890";
        String path = PathSanitizer.hashPath(hash);
        assertTrue(path.startsWith("ab/"));
        assertEquals("ab/" + hash.substring(2), path);
    }

    @Test
    void sanitizeConfigFile() {
        assertEquals("config/sodium-options.json", PathSanitizer.sanitize("config/sodium-options.json"));
    }

    @Test
    void sanitizeDeeplyNested() {
        assertEquals("a/b/c/d/e", PathSanitizer.sanitize("a/b/c/d/e"));
    }
}
