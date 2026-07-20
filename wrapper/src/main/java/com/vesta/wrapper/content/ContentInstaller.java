package com.vesta.wrapper.content;

import com.vesta.sync.hashing.FileHasher;
import com.vesta.sync.util.PathSanitizer;
import org.slf4j.Logger;
import org.slf4j.LoggerFactory;

import java.io.IOException;
import java.io.InputStream;
import java.nio.file.Files;
import java.nio.file.Path;
import java.nio.file.StandardCopyOption;
import java.util.Objects;
import java.util.Optional;

/**
 * Serves and writes files directly under the scan root — no content-addressable blob mirror.
 */
public final class ContentInstaller {

    private static final Logger log = LoggerFactory.getLogger(ContentInstaller.class);

    private final Path scanRoot;

    public ContentInstaller(Path scanRoot) {
        this.scanRoot = scanRoot.toAbsolutePath().normalize();
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

    public Optional<Path> resolveLivePath(String relativePath) {
        if (relativePath == null || relativePath.trim().isEmpty()) {
            return Optional.empty();
        }
        String normalized = PathSanitizer.sanitize(relativePath);
        Path target = scanRoot.resolve(normalized).normalize();
        if (!target.startsWith(scanRoot) || !Files.isRegularFile(target)) {
            return Optional.empty();
        }
        return Optional.of(target);
    }

    public Optional<Path> resolveByHash(String sha256, Iterable<com.vesta.sync.manifest.ManifestFile> manifestFiles) {
        if (sha256 == null) {
            return Optional.empty();
        }
        String want = sha256.toLowerCase();
        for (com.vesta.sync.manifest.ManifestFile m : manifestFiles) {
            if (want.equalsIgnoreCase(m.sha256)) {
                return resolveLivePath(m.path);
            }
        }
        return Optional.empty();
    }

    public static final class Result {
        public final String path;
        public final String sha256;
        public final long size;

        public Result(String path, String sha256, long size) {
            this.path = path;
            this.sha256 = sha256;
            this.size = size;
        }
    }
}
