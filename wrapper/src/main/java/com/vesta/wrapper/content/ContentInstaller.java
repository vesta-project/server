package com.vesta.wrapper.content;

import com.vesta.sync.hashing.FileHasher;
import com.vesta.wrapper.files.ServerPathPolicy;
import org.slf4j.Logger;
import org.slf4j.LoggerFactory;

import java.io.IOException;
import java.io.InputStream;
import java.nio.file.Files;
import java.nio.file.Path;
import java.nio.file.attribute.BasicFileAttributes;
import java.util.Collection;
import java.util.List;
import java.util.Objects;
import java.util.Optional;

/**
 * Serves and writes files directly under the scan root — no content-addressable blob mirror.
 * All paths are confined by {@link ServerPathPolicy}.
 */
public final class ContentInstaller {

    private static final Logger log = LoggerFactory.getLogger(ContentInstaller.class);

    private final Path scanRoot;
    private final ServerPathPolicy policy;

    public ContentInstaller(Path scanRoot) {
        this(scanRoot, List.of());
    }

    /**
     * @param extraProtectedPaths additional paths (e.g. TLS keystore, private state roots) denied on
     *                            top of {@link ServerPathPolicy#PROTECTED_NAMES}.
     */
    public ContentInstaller(Path scanRoot, Collection<Path> extraProtectedPaths) {
        this.scanRoot = scanRoot.toAbsolutePath().normalize();
        this.policy = new ServerPathPolicy(this.scanRoot, extraProtectedPaths);
    }

    public Result install(String relativePath, InputStream data) throws IOException {
        Objects.requireNonNull(relativePath, "relativePath");
        String normalized = ServerPathPolicy.normalize(relativePath, false);
        Path target = policy.resolve(normalized, false);
        policy.createDirectories(target.getParent());
        ServerPathPolicy.atomicReplace(target, data);
        String sha = FileHasher.sha256(target);
        log.info("Installed {} (sha256={})", normalized, sha);
        return new Result(normalized, sha, Files.size(target));
    }

    public boolean remove(String relativePath) throws IOException {
        String normalized = ServerPathPolicy.normalize(relativePath, false);
        Path target = policy.resolve(normalized, false);
        BasicFileAttributes attrs = ServerPathPolicy.readNoFollow(target);
        if (attrs == null) {
            return false;
        }
        if (!attrs.isRegularFile()) {
            throw new IOException("not a file: " + normalized);
        }
        boolean deleted = Files.deleteIfExists(target);
        if (deleted) {
            log.info("Removed {}", normalized);
        }
        return deleted;
    }

    /** Returns the confined live file, or empty when the path is invalid, protected, linked, or absent. */
    public Optional<Path> resolveLivePath(String relativePath) {
        if (relativePath == null || relativePath.trim().isEmpty()) {
            return Optional.empty();
        }
        try {
            Path target = policy.resolve(relativePath, false);
            BasicFileAttributes attrs = ServerPathPolicy.readNoFollow(target);
            if (attrs == null || !attrs.isRegularFile()) {
                return Optional.empty();
            }
            return Optional.of(target);
        } catch (SecurityException | IOException e) {
            return Optional.empty();
        }
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
