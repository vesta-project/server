package com.vesta.sync.hashing;

import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.io.TempDir;

import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;

import static org.junit.jupiter.api.Assertions.*;

class FileHasherTest {

    @Test
    void sha256Deterministic() {
        byte[] data = "hello world".getBytes(StandardCharsets.UTF_8);
        String h1 = FileHasher.sha256(data);
        String h2 = FileHasher.sha256(data);
        assertEquals(h1, h2);
        assertEquals(64, h1.length());
    }

    @Test
    void sha256ByteArrayKnown() {
        String hash = FileHasher.sha256("hello".getBytes(StandardCharsets.UTF_8));
        assertEquals("2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824", hash);
    }

    @Test
    void sha256FromFile(@TempDir Path tmp) throws Exception {
        Path file = tmp.resolve("test.txt");
        Files.write(file, "hello".getBytes(StandardCharsets.UTF_8));
        String hash = FileHasher.sha256(file);
        assertEquals("2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824", hash);
    }

    @Test
    void verifySuccess(@TempDir Path tmp) throws Exception {
        Path file = tmp.resolve("test.txt");
        Files.write(file, "hello".getBytes(StandardCharsets.UTF_8));
        String hash = FileHasher.sha256(file);
        assertTrue(FileHasher.verify(file, hash));
    }

    @Test
    void verifyFailure(@TempDir Path tmp) throws Exception {
        Path file = tmp.resolve("test.txt");
        Files.write(file, "hello".getBytes(StandardCharsets.UTF_8));
        assertFalse(FileHasher.verify(file, "deadbeef"));
    }

    @Test
    void sha256EmptyData() {
        String hash = FileHasher.sha256(new byte[0]);
        assertEquals("e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855", hash);
    }
}
