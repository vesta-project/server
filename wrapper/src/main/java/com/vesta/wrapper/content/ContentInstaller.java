package com.vesta.wrapper.content;

import com.vesta.sync.FileStore;
import com.vesta.sync.hashing.FileHasher;
import com.vesta.sync.util.PathSanitizer;
import org.slf4j.Logger;
import org.slf4j.LoggerFactory;

import java.io.IOException;
import java.io.InputStream;
import java.nio.file.Files;
import java.nio.file.Path;
import java.nio.file.StandardCopyOption;
import java.util.Locale;
import java.util.Objects;

/**
 * Writes/removes files under the scan root (e.g. mods/) while the game may be running.
 */
public final class ContentInstaller {

    private static final Logger log = LoggerFactory.getLogger(ContentInstaller.class);

    private final Path scanRoot;
    private final FileStore fileStore;

    public ContentInstaller(Path scanRoot, FileStore fileStore) {
        this.scanRoot = scanRoot.toAbsolutePath().normalize();
        this.fileStore = fileStore;
    }

    public Result install(String relativePath, InputStream data) throws IOException {
        Objects.requireNonNull(relativePath, "relativePath");
        String normalized = PathSanitizer.sanitize(relativePath);
        Path target = scanRoot.resolve(normalized).normalize();
        if (!target.startsWith(scanRoot)) {
            throw new SecurityException("Path escapes scan root");
        }
        Files.createDirectories(target.getParent());
        Path tmp = Files.createTempFile(target.getParent(), ".vesta-", ".part");
        try {
            Files.copy(data, tmp, StandardCopyOption.REPLACE_EXISTING);
            try {
                Files.move(tmp, target, StandardCopyOption.REPLACE_EXISTING, StandardCopyOption.ATOMIC_MOVE);
            } catch (IOException e) {
                Files.move(tmp, target, StandardCopyOption.REPLACE_EXISTING);
            }
        } catch (IOException e) {
            Files.deleteIfExists(tmp);
            throw e;
        }
        String sha = FileHasher.sha256(target);
        fileStore.store(target);
        log.info("Installed {} (sha256={})", normalized, sha);
        return new Result(normalized, sha, Files.size(target));
    }

    public boolean remove(String relativePath) throws IOException {
        String normalized = PathSanitizer.sanitize(relativePath);
        Path target = scanRoot.resolve(normalized).normalize();
        if (!target.startsWith(scanRoot)) {
            throw new SecurityException("Path escapes scan root");
        }
        boolean deleted = Files.deleteIfExists(target);
        if (deleted) {
            log.info("Removed {}", normalized);
        }
        return deleted;
    }

    public record Result(String path, String sha256, long size) {}
}
