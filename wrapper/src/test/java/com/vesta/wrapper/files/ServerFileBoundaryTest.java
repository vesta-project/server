package com.vesta.wrapper.files;

import com.vesta.wrapper.config.WrapperConfig;
import com.vesta.wrapper.content.ContentInstaller;
import com.vesta.wrapper.server.ServerState;
import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.io.TempDir;
import org.junit.jupiter.params.ParameterizedTest;
import org.junit.jupiter.params.provider.ValueSource;

import java.io.ByteArrayInputStream;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.List;

import static org.junit.jupiter.api.Assertions.*;

class ServerFileBoundaryTest {
    @TempDir Path root;

    private ByteArrayInputStream payload() {
        return new ByteArrayInputStream("replacement".getBytes(StandardCharsets.UTF_8));
    }

    private FileBrowserService browser(Path game, List<Path> protectedPaths) {
        WrapperConfig config = new WrapperConfig.Builder().scanRoot(game)
                .scanPaths(List.of()).minecraftWorkingDirectory(game).build();
        return new FileBrowserService(game, game, List.of(), new ServerState(config), protectedPaths);
    }

    @ParameterizedTest
    @ValueSource(strings = {"../secret", "config/../secret", "config//secret", "C:/secret", "C:secret",
            "//host/share", "\\\\host\\share", "/secret", "config/file:stream", "NUL.txt", "CON",
            "COM1.log", "LPT²", "config/trailing.", "config/trailing ", "config/line\nfeed", "config/nul\u0000"})
    void fileAndContentWritesRejectUnsafePathsWithoutEffects(String path) throws Exception {
        FileBrowserService files = browser(root, List.of());
        ContentInstaller content = new ContentInstaller(root);
        assertThrows(SecurityException.class, () -> files.write(path, payload()));
        assertThrows(SecurityException.class, () -> content.install(path, payload()));
        assertThrows(SecurityException.class, () -> content.remove(path));
        assertTrue(content.resolveLivePath(path).isEmpty());
    }

    @ParameterizedTest
    @ValueSource(strings = {".vesta/keys.pem", ".VESTA/tokens.json", ".git/config", "nested/.hg/key",
            "nested/.svn/key", "wrapper-config.toml", "WRAPPER-CONFIG.TOML", "vesta-wrapper/agent.jar",
            "start-vesta-wrapper.bat", "start-vesta-wrapper.sh"})
    void protectedPathsAreDeniedAcrossEveryFileAndContentOperation(String path) throws Exception {
        FileBrowserService files = browser(root, List.of());
        ContentInstaller content = new ContentInstaller(root);
        assertThrows(SecurityException.class, () -> files.resolveFile(path));
        assertThrows(SecurityException.class, () -> files.write(path, payload()));
        assertThrows(SecurityException.class, () -> files.delete(path));
        assertThrows(SecurityException.class, () -> files.list(path));
        assertThrows(SecurityException.class, () -> content.install(path, payload()));
        assertThrows(SecurityException.class, () -> content.remove(path));
        assertTrue(content.resolveLivePath(path).isEmpty());
    }

    @Test
    void customTlsAndPrivateStateRootsAreProtectedButOrdinaryDotfilesRemainVisible() throws Exception {
        Path privateState = Files.createDirectories(root.resolve("private-state"));
        Path tls = root.resolve("server-identity.p12");
        Files.writeString(tls, "secret");
        Files.writeString(privateState.resolve("vault"), "secret");
        Files.writeString(root.resolve(".ordinary-config"), "visible");
        Files.createDirectories(root.resolve(".git"));
        FileBrowserService files = browser(root, List.of(privateState, tls));
        ContentInstaller content = new ContentInstaller(root, List.of(privateState, tls));
        var names = files.list("").entries.stream().map(e -> e.name).toList();
        assertTrue(names.contains(".ordinary-config"));
        assertFalse(names.contains(".git"));
        assertFalse(names.contains("private-state"));
        assertFalse(names.contains("server-identity.p12"));
        for (String path : List.of("private-state/vault", "server-identity.p12", "SERVER-IDENTITY.P12")) {
            assertThrows(SecurityException.class, () -> files.resolveFile(path));
            assertThrows(SecurityException.class, () -> files.write(path, payload()));
            assertThrows(SecurityException.class, () -> content.install(path, payload()));
        }
        assertEquals("secret", Files.readString(tls));
    }

    @Test
    void junctionOrSymlinkParentCannotCreateMissingFileOutsideGameRoot() throws Exception {
        Path game = Files.createDirectories(root.resolve("game"));
        Path external = Files.createDirectories(root.resolve("outside"));
        Files.writeString(external.resolve("existing.txt"), "original");
        Path link = game.resolve("linked");
        createDirectoryLink(link, external);
        try {
            FileBrowserService files = browser(game, List.of());
            ContentInstaller content = new ContentInstaller(game);
            assertThrows(SecurityException.class, () -> files.write("linked/new.txt", payload()));
            assertThrows(SecurityException.class, () -> content.install("linked/nested/new.txt", payload()));
            assertThrows(SecurityException.class, () -> files.resolveFile("linked/existing.txt"));
            assertThrows(SecurityException.class, () -> content.remove("linked/existing.txt"));
            assertFalse(Files.exists(external.resolve("new.txt")));
            assertFalse(Files.exists(external.resolve("nested")));
            assertEquals("original", Files.readString(external.resolve("existing.txt")));
        } finally {
            Files.deleteIfExists(link);
        }
    }

    @Test
    void failedUploadLeavesPreviousFileAndNoTemporaryReplacement() throws Exception {
        Files.writeString(root.resolve("config.txt"), "original");
        FileBrowserService files = browser(root, List.of());
        var failing = new java.io.InputStream() {
            @Override public int read() throws java.io.IOException { throw new java.io.IOException("injected"); }
        };
        assertThrows(java.io.IOException.class, () -> files.write("config.txt", failing));
        assertEquals("original", Files.readString(root.resolve("config.txt")));
        try (var children = Files.list(root)) {
            assertFalse(children.anyMatch(p -> p.getFileName().toString().endsWith(".part")));
        }
    }

    private void createDirectoryLink(Path link, Path target) throws Exception {
        com.vesta.wrapper.testutil.NativeTestLinks.directory(link, target);
    }
}
