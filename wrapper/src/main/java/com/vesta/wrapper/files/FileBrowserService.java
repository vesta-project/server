package com.vesta.wrapper.files;

import com.vesta.wrapper.server.ServerState;

import java.io.IOException;
import java.io.InputStream;
import java.nio.file.FileVisitResult;
import java.nio.file.Files;
import java.nio.file.LinkOption;
import java.nio.file.Path;
import java.nio.file.SimpleFileVisitor;
import java.nio.file.attribute.BasicFileAttributes;
import java.time.Instant;
import java.util.ArrayList;
import java.util.Collection;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import java.util.Objects;

/**
 * Lists, reads, writes, and deletes files under the Minecraft server working directory.
 * All paths are confined by {@link ServerPathPolicy}.
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
    private final ServerPathPolicy policy;

    public FileBrowserService(
            Path workingDirectory,
            Path configuredScanRoot,
            List<String> scanPaths,
            ServerState serverState
    ) {
        this(workingDirectory, configuredScanRoot, scanPaths, serverState, List.of());
    }

    /**
     * @param extraProtectedPaths additional paths (e.g. a TLS keystore or private state root) that must
     *                            never be listed, read, written, or deleted, on top of
     *                            {@link ServerPathPolicy#PROTECTED_NAMES}.
     */
    public FileBrowserService(
            Path workingDirectory,
            Path configuredScanRoot,
            List<String> scanPaths,
            ServerState serverState,
            Collection<Path> extraProtectedPaths
    ) {
        this.root = workingDirectory.toAbsolutePath().normalize();
        this.scanRoot = resolveScanRoot(configuredScanRoot);
        this.scanPaths = scanPaths != null ? List.copyOf(scanPaths) : List.of();
        this.serverState = Objects.requireNonNull(serverState, "serverState");
        this.policy = new ServerPathPolicy(root, extraProtectedPaths);
    }

    public Path root() {
        return root;
    }

    public Listing list(String relativePath) throws IOException {
        String normalized = ServerPathPolicy.normalize(stripLeadingSlashes(relativePath), true);
        Path dir = policy.resolve(normalized, true);
        BasicFileAttributes dirAttrs = ServerPathPolicy.readNoFollow(dir);
        if (dirAttrs == null || !dirAttrs.isDirectory()) {
            throw new IOException("not a directory: " + normalized);
        }
        List<Entry> entries = new ArrayList<>();
        try (var stream = Files.list(dir)) {
            stream.sorted((left, right) -> left.getFileName().toString()
                    .compareToIgnoreCase(right.getFileName().toString()))
                    .forEach(path -> {
                        if (policy.isHidden(path)) {
                            return;
                        }
                        String name = path.getFileName().toString();
                        try {
                            BasicFileAttributes attrs = ServerPathPolicy.readNoFollow(path);
                            if (attrs == null) {
                                return;
                            }
                            // Omits symlinks, junctions, and any DOS reparse point, not just basic link flags.
                            ServerPathPolicy.assertPlain(path, attrs);
                            if (attrs.isDirectory()) {
                                entries.add(new Entry(name, "dir", 0,
                                        formatModified(attrs.lastModifiedTime().toInstant())));
                            } else if (attrs.isRegularFile()) {
                                entries.add(new Entry(name, "file", attrs.size(),
                                        formatModified(attrs.lastModifiedTime().toInstant())));
                            }
                        } catch (IOException | SecurityException ignored) {
                            // skip unreadable, linked, or special entries
                        }
                    });
        }
        return new Listing(normalized, entries);
    }

    /**
     * Resolves an existing regular file. Prefer {@link #openRead(String)}, which additionally refuses
     * to follow a link swapped in at the final component after validation.
     */
    public Path resolveFile(String relativePath) throws IOException {
        Path resolved = policy.resolve(relativePath, false);
        BasicFileAttributes attrs = ServerPathPolicy.readNoFollow(resolved);
        if (attrs == null || !attrs.isRegularFile()) {
            throw new IOException("not a file: " + ServerPathPolicy.normalize(relativePath, false));
        }
        return resolved;
    }

    /** Opens an existing regular file for reading without following a final-component link. */
    public InputStream openRead(String relativePath) throws IOException {
        return Files.newInputStream(resolveFile(relativePath), LinkOption.NOFOLLOW_LINKS);
    }

    public void write(String relativePath, InputStream data) throws IOException {
        Path target = policy.resolve(relativePath, false);
        policy.createDirectories(target.getParent());
        ServerPathPolicy.atomicReplace(target, data);
        maybeRescan(target);
    }

    public void delete(String relativePath) throws IOException {
        Path target = policy.resolve(relativePath, false);
        BasicFileAttributes attrs = ServerPathPolicy.readNoFollow(target);
        if (attrs == null) {
            throw new IOException("file not found: " + ServerPathPolicy.normalize(relativePath, false));
        }
        if (attrs.isDirectory()) {
            throw new IOException("directories cannot be deleted yet");
        }
        if (!Files.deleteIfExists(target)) {
            throw new IOException("file not found: " + ServerPathPolicy.normalize(relativePath, false));
        }
        maybeRescan(target);
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
        Path effectiveScanRoot = scanRoot;
        if (!written.startsWith(effectiveScanRoot)) {
            try {
                effectiveScanRoot = scanRoot.toRealPath();
            } catch (IOException e) {
                return;
            }
            if (!written.startsWith(effectiveScanRoot)) {
                return;
            }
        }
        String relative = effectiveScanRoot.relativize(written).toString().replace('\\', '/');
        for (String scanPath : scanPaths) {
            String normalizedScan = scanPath.replace('\\', '/');
            if (relative.equals(normalizedScan) || relative.startsWith(normalizedScan + "/")) {
                serverState.rescan();
                return;
            }
        }
    }

    /**
     * Listing historically accepted "/" and "/dir" as root-relative; strip exactly one forward slash.
     * Any further prefix ("//", "/\", "\\server", "\dir") is left for {@link ServerPathPolicy#normalize}
     * to reject as absolute/UNC.
     */
    private static String stripLeadingSlashes(String relativePath) {
        if (relativePath != null && relativePath.startsWith("/")) {
            return relativePath.substring(1);
        }
        return relativePath;
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
