package com.vesta.wrapper.files;

import com.vesta.sync.util.PathSanitizer;
import com.vesta.wrapper.server.ServerState;

import java.io.IOException;
import java.io.InputStream;
import java.nio.file.FileVisitResult;
import java.nio.file.Files;
import java.nio.file.Path;
import java.nio.file.SimpleFileVisitor;
import java.nio.file.attribute.BasicFileAttributes;
import java.time.Instant;
import java.util.ArrayList;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Locale;
import java.util.Map;
import java.util.Objects;

/**
 * Lists, reads, writes, and deletes files under the Minecraft server working directory.
 */
public final class FileBrowserService {

    public static final class Entry {
        public final String name;
        public final String type;
        public final long size;
        public final String modifiedAt;

        public Entry(String name, String type, long size, String modifiedAt) {
            this.name = name;
            this.type = type;
            this.size = size;
            this.modifiedAt = modifiedAt;
        }
    }

    public static final class Listing {
        public final String path;
        public final List<Entry> entries;

        public Listing(String path, List<Entry> entries) {
            this.path = path;
            this.entries = entries;
        }
    }

    private final Path root;
    private final Path scanRoot;
    private final List<String> scanPaths;
    private final ServerState serverState;

    public FileBrowserService(
            Path workingDirectory,
            Path configuredScanRoot,
            List<String> scanPaths,
            ServerState serverState
    ) {
        this.root = workingDirectory.toAbsolutePath().normalize();
        this.scanRoot = resolveScanRoot(configuredScanRoot);
        this.scanPaths = scanPaths != null ? List.copyOf(scanPaths) : List.of();
        this.serverState = Objects.requireNonNull(serverState, "serverState");
    }

    public Path root() {
        return root;
    }

    public Listing list(String relativePath) throws IOException {
        Path dir = resolveDirectory(relativePath);
        List<Entry> entries = new ArrayList<>();
        try (var stream = Files.list(dir)) {
            stream.sorted((left, right) -> left.getFileName().toString()
                    .compareToIgnoreCase(right.getFileName().toString()))
                    .forEach(path -> {
                        String name = path.getFileName().toString();
                        if (name.startsWith(".")) {
                            return;
                        }
                        try {
                            if (Files.isDirectory(path)) {
                                entries.add(new Entry(
                                        name,
                                        "dir",
                                        0,
                                        formatModified(Files.getLastModifiedTime(path).toInstant())
                                ));
                            } else if (Files.isRegularFile(path)) {
                                entries.add(new Entry(
                                        name,
                                        "file",
                                        Files.size(path),
                                        formatModified(Files.getLastModifiedTime(path).toInstant())
                                ));
                            }
                        } catch (IOException ignored) {
                            // skip unreadable entries
                        }
                    });
        }
        return new Listing(normalizeRelative(relativePath), entries);
    }

    public Path resolveFile(String relativePath) throws IOException {
        Path resolved = resolvePath(relativePath);
        if (!Files.isRegularFile(resolved)) {
            throw new IOException("not a file: " + normalizeRelative(relativePath));
        }
        return resolved;
    }

    public void write(String relativePath, InputStream data) throws IOException {
        String normalized = PathSanitizer.sanitize(relativePath);
        Path target = resolvePath(normalized);
        Files.createDirectories(target.getParent());
        Path tmp = Files.createTempFile(target.getParent(), ".vesta-", ".part");
        try {
            Files.copy(data, tmp, java.nio.file.StandardCopyOption.REPLACE_EXISTING);
            try {
                Files.move(tmp, target,
                        java.nio.file.StandardCopyOption.REPLACE_EXISTING,
                        java.nio.file.StandardCopyOption.ATOMIC_MOVE);
            } catch (IOException moveError) {
                Files.move(tmp, target, java.nio.file.StandardCopyOption.REPLACE_EXISTING);
            }
        } catch (IOException error) {
            Files.deleteIfExists(tmp);
            throw error;
        }
        maybeRescan(target);
    }

    public void delete(String relativePath) throws IOException {
        Path target = resolvePath(relativePath);
        if (Files.isDirectory(target)) {
            throw new IOException("directories cannot be deleted yet");
        }
        if (!Files.deleteIfExists(target)) {
            throw new IOException("file not found: " + normalizeRelative(relativePath));
        }
        maybeRescan(target);
    }

    private Path resolveDirectory(String relativePath) throws IOException {
        String normalized = normalizeRelative(relativePath);
        Path resolved = normalized.isEmpty() ? root : resolvePath(normalized);
        if (!Files.isDirectory(resolved)) {
            throw new IOException("not a directory: " + normalized);
        }
        return resolved;
    }

    private Path resolvePath(String relativePath) throws IOException {
        String normalized = PathSanitizer.sanitize(relativePath);
        Path resolved = root.resolve(normalized).normalize();
        if (!resolved.startsWith(root)) {
            throw new SecurityException("path escapes server root");
        }
        if (Files.exists(resolved)) {
            Path real = resolved.toRealPath();
            if (!real.startsWith(root)) {
                throw new SecurityException("path escapes server root");
            }
            return real;
        }
        return resolved;
    }

    private Path resolveScanRoot(Path configuredScanRoot) {
        Path candidate = configuredScanRoot != null
                ? configuredScanRoot
                : root;
        if (!candidate.isAbsolute()) {
            candidate = root.resolve(candidate);
        }
        return candidate.toAbsolutePath().normalize();
    }

    private void maybeRescan(Path written) {
        if (!written.startsWith(scanRoot)) {
            return;
        }
        String relative = scanRoot.relativize(written).toString().replace('\\', '/');
        for (String scanPath : scanPaths) {
            String normalizedScan = scanPath.replace('\\', '/');
            if (relative.equals(normalizedScan) || relative.startsWith(normalizedScan + "/")) {
                serverState.rescan();
                return;
            }
        }
    }

    private static String normalizeRelative(String relativePath) {
        if (relativePath == null || relativePath.isBlank()) {
            return "";
        }
        return relativePath.replace('\\', '/').replaceAll("^/+", "").replaceAll("/+$", "");
    }

    private static String formatModified(Instant instant) {
        return instant != null ? instant.toString() : "";
    }

    /** Deletes every file under {@code root} for isolated tests. */
    public void deleteTreeForTests() throws IOException {
        if (!Files.isDirectory(root)) {
            return;
        }
        Files.walkFileTree(root, new SimpleFileVisitor<>() {
            @Override
            public FileVisitResult visitFile(Path file, BasicFileAttributes attrs) throws IOException {
                Files.delete(file);
                return FileVisitResult.CONTINUE;
            }

            @Override
            public FileVisitResult postVisitDirectory(Path dir, IOException exc) throws IOException {
                if (!dir.equals(root)) {
                    Files.delete(dir);
                }
                return FileVisitResult.CONTINUE;
            }
        });
    }

    public Map<String, Object> listingToMap(Listing listing) {
        List<Map<String, Object>> entries = new ArrayList<>();
        for (Entry entry : listing.entries) {
            Map<String, Object> row = new LinkedHashMap<>();
            row.put("name", entry.name);
            row.put("type", entry.type);
            row.put("size", entry.size);
            row.put("modified_at", entry.modifiedAt);
            entries.add(row);
        }
        Map<String, Object> out = new LinkedHashMap<>();
        out.put("path", listing.path);
        out.put("entries", entries);
        return out;
    }
}
