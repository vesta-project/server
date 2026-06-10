package com.vesta.sync.manifest;

import com.vesta.sync.FileStore;
import com.vesta.sync.hashing.FileHasher;
import com.vesta.sync.util.PathSanitizer;
import org.slf4j.Logger;
import org.slf4j.LoggerFactory;

import java.io.IOException;
import java.nio.file.FileVisitResult;
import java.nio.file.Files;
import java.nio.file.Path;
import java.nio.file.SimpleFileVisitor;
import java.nio.file.attribute.BasicFileAttributes;
import java.util.ArrayList;
import java.util.Arrays;
import java.util.Collections;
import java.util.List;

public final class ManifestBuilder {

    private static final Logger log = LoggerFactory.getLogger(ManifestBuilder.class);

    public static final List<String> DEFAULT_SCAN_DIRS =
            Collections.unmodifiableList(Arrays.asList("mods", "config"));

    private ManifestBuilder() {}

    public static ScanResult scan(Path root, List<String> scanDirs, FileStore store) throws IOException {
        if (scanDirs == null || scanDirs.isEmpty()) {
            scanDirs = DEFAULT_SCAN_DIRS;
        }

        Path normalizedRoot = root.toAbsolutePath().normalize();
        if (!Files.isDirectory(normalizedRoot)) {
            throw new IOException("Scan root is not a directory: " + normalizedRoot);
        }

        List<ManifestFile> mods = new ArrayList<>();
        List<ManifestFile> overrides = new ArrayList<>();

        for (String scanDir : scanDirs) {
            String sanitizedDir = PathSanitizer.sanitize(scanDir.replace('\\', '/'));
            Path dir = normalizedRoot.resolve(sanitizedDir).normalize();
            if (!dir.startsWith(normalizedRoot)) {
                throw new IOException("Scan path escapes root: " + scanDir);
            }
            if (!Files.isDirectory(dir)) {
                log.debug("Scan path missing, skipping: {}", dir);
                continue;
            }

            Files.walkFileTree(dir, new SimpleFileVisitor<Path>() {
                @Override
                public FileVisitResult preVisitDirectory(Path dir, BasicFileAttributes attrs) {
                    String name = dir.getFileName().toString();
                    if (name.startsWith(".")) {
                        return FileVisitResult.SKIP_SUBTREE;
                    }
                    return FileVisitResult.CONTINUE;
                }

                @Override
                public FileVisitResult visitFile(Path file, BasicFileAttributes attrs) throws IOException {
                    if (!attrs.isRegularFile()) {
                        return FileVisitResult.CONTINUE;
                    }
                    String fileName = file.getFileName().toString();
                    if (fileName.startsWith(".")) {
                        return FileVisitResult.CONTINUE;
                    }

                    String relative = normalizedRoot.relativize(file).toString().replace('\\', '/');
                    relative = PathSanitizer.sanitize(relative);

                    ManifestFile entry = buildEntry(file, store);
                    entry.path = relative;
                    if (relative.startsWith("mods/")) {
                        mods.add(entry);
                    } else {
                        overrides.add(entry);
                    }
                    return FileVisitResult.CONTINUE;
                }
            });
        }

        mods.sort((a, b) -> a.path.compareTo(b.path));
        overrides.sort((a, b) -> a.path.compareTo(b.path));

        log.info("Manifest scan complete: {} mods, {} overrides from {}", mods.size(), overrides.size(), normalizedRoot);
        return new ScanResult(mods, overrides);
    }

    private static ManifestFile buildEntry(Path file, FileStore store) throws IOException {
        store.store(file);

        ManifestFile entry = new ManifestFile();
        entry.sha256 = FileHasher.sha256(file);
        entry.size = Files.size(file);
        entry.source = serverSource();
        return entry;
    }

    private static FileSource serverSource() {
        FileSource source = new FileSource();
        source.type = "server";
        source.url = null;
        return source;
    }

    public static final class ScanResult {
        public final List<ManifestFile> mods;
        public final List<ManifestFile> overrides;

        public ScanResult(List<ManifestFile> mods, List<ManifestFile> overrides) {
            this.mods = Collections.unmodifiableList(new ArrayList<>(mods));
            this.overrides = Collections.unmodifiableList(new ArrayList<>(overrides));
        }
    }
}
