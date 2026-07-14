package com.vesta.wrapper.content;

import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.io.TempDir;

import java.io.ByteArrayInputStream;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;

import static org.junit.jupiter.api.Assertions.*;

class ContentInstallerTest {

    @TempDir
    Path temp;

    @Test
    void installAndRemove() throws Exception {
        Path scan = temp.resolve("server");
        Files.createDirectories(scan.resolve("mods"));
        ContentInstaller installer = new ContentInstaller(scan);

        byte[] data = "fake-mod".getBytes(StandardCharsets.UTF_8);
        ContentInstaller.Result result = installer.install("mods/demo.jar", new ByteArrayInputStream(data));
        assertEquals("mods/demo.jar", result.path);
        assertTrue(Files.exists(scan.resolve("mods/demo.jar")));
        assertNotNull(result.sha256);

        assertTrue(installer.remove("mods/demo.jar"));
        assertFalse(Files.exists(scan.resolve("mods/demo.jar")));
    }

    @Test
    void rejectsTraversal() {
        Path scan = temp.resolve("server");
        ContentInstaller installer = new ContentInstaller(scan);
        assertThrows(IllegalArgumentException.class, () ->
                installer.install("../escape.jar", new ByteArrayInputStream(new byte[]{1})));
    }
}
