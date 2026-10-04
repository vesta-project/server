package com.vesta.wrapper.files;

import com.vesta.wrapper.config.WrapperConfig;
import com.vesta.wrapper.server.ServerState;
import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.io.TempDir;

import java.io.ByteArrayInputStream;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.List;

import static org.junit.jupiter.api.Assertions.assertArrayEquals;
import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertThrows;
import static org.junit.jupiter.api.Assertions.assertTrue;

class FileBrowserServiceTest {

    @TempDir
    Path root;

    @Test
    void listsWritesReadsAndDeletesFiles() throws Exception {
        Files.writeString(root.resolve("readme.txt"), "hello", StandardCharsets.UTF_8);
        FileBrowserService browser = browser(root, List.of());
        FileBrowserService.Listing listing = browser.list("");
        assertTrue(listing.entries.stream().anyMatch(entry -> "readme.txt".equals(entry.name)));

        browser.write("nested/data.txt", new ByteArrayInputStream("payload".getBytes(StandardCharsets.UTF_8)));
        assertArrayEquals(
                "payload".getBytes(StandardCharsets.UTF_8),
                Files.readAllBytes(browser.resolveFile("nested/data.txt"))
        );

        browser.delete("nested/data.txt");
        assertThrows(Exception.class, () -> browser.resolveFile("nested/data.txt"));
    }

    @Test
    void rejectsTraversal() {
        FileBrowserService browser = browser(root, List.of());
        assertThrows(SecurityException.class, () -> browser.list("../secrets"));
    }

    private static FileBrowserService browser(Path root, List<String> scanPaths) {
        WrapperConfig config = new WrapperConfig.Builder()
                .scanRoot(root)
                .scanPaths(scanPaths)
                .minecraftWorkingDirectory(root)
                .build();
        return new FileBrowserService(root, root, scanPaths, new ServerState(config));
    }
}
