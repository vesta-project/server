package com.vesta.sync.manifest;

import com.vesta.sync.hashing.FileHasher;

import java.io.IOException;
import java.nio.file.Files;
import java.nio.file.Path;
import java.nio.file.Paths;
import java.util.Locale;

/**
 * Validates and fingerprints a locally stored pack archive under the server root.
 * The archive is served as a single authenticated blob — contents are never extracted.
 */
public final class LocalArchivePack {

    public final Path resolvedPath;
    public final String relativePath;
    public final String sha256;
    public final long size;
    /** {@code mrpack} or {@code zip}. */
    public final String format;

    LocalArchivePack(
            Path resolvedPath,
            String relativePath,
            String sha256,
            long size,
            String format
    ) {
        this.resolvedPath = resolvedPath;
        this.relativePath = relativePath;
        this.sha256 = sha256;
        this.size = size;
        this.format = format;
    }

    public static LocalArchivePack of(
            Path resolvedPath,
            String relativePath,
            String sha256,
            long size,
            String format
    ) {
        return new LocalArchivePack(resolvedPath, relativePath, sha256, size, format);
    }

    public static LocalArchivePack resolve(Path scanRoot, String configuredPath) throws IOException {
        if (configuredPath == null || configuredPath.trim().isEmpty()) {
            throw new IllegalArgumentException(
                    "modpack.archive.path is required when modpack.source = \"archive\""
            );
        }
        if (scanRoot == null) {
            throw new IllegalArgumentException("scan root is required");
        }

        Path root = scanRoot.toAbsolutePath().normalize();
        Path configured = Paths.get(configuredPath.trim());
        Path candidate = configured.isAbsolute()
                ? configured.normalize()
                : root.resolve(configured).normalize();

        if (!candidate.startsWith(root)) {
            throw new IllegalArgumentException(
                    "Archive path escapes the server root: " + configuredPath
            );
        }
        if (!Files.exists(candidate)) {
            throw new IllegalArgumentException("Archive file not found: " + configuredPath);
        }
        if (Files.isSymbolicLink(candidate)) {
            throw new IllegalArgumentException(
                    "Archive path must not be a symbolic link: " + configuredPath
            );
        }
        if (!Files.isRegularFile(candidate)) {
            throw new IllegalArgumentException(
                    "Archive path must be a regular file: " + configuredPath
            );
        }

        String fileName = candidate.getFileName().toString().toLowerCase(Locale.ROOT);
        String format;
        if (fileName.endsWith(".mrpack")) {
            format = "mrpack";
        } else if (fileName.endsWith(".zip")) {
            format = "zip";
        } else {
            throw new IllegalArgumentException(
                    "Archive must be a .zip or .mrpack file: " + configuredPath
            );
        }

        String relative = root.relativize(candidate).toString().replace('\\', '/');
        return new LocalArchivePack(
                candidate,
                relative,
                FileHasher.sha256(candidate),
                Files.size(candidate),
                format
        );
    }
}
