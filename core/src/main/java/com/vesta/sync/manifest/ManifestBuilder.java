package com.vesta.sync.manifest;

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

/**
 * Builds public (known) and private (unknown) file indexes from the scan tree without duplicating bytes.
 */
public final class ManifestBuilder {

    private static final Logger log = LoggerFactory.getLogger(ManifestBuilder.class);

    public static final List<String> DEFAULT_SCAN_DIRS =
            Collections.unmodifiableList(Arrays.asList("mods", "config"));

    private ManifestBuilder() {}

    public static ScanResult scan(
            Path root,
            List<String> scanDirs,
            KnownSourceRegistry known,
            UnresolvedPolicy unresolvedPolicy,
            List<String> extraPrivatePaths
    ) throws IOException {
        if (scanDirs == null || scanDirs.isEmpty()) {
            scanDirs = DEFAULT_SCAN_DIRS;
        }

        Path normalizedRoot = root.toAbsolutePath().normalize();
        if (!Files.isDirectory(normalizedRoot)) {
            throw new IOException("Scan root is not a directory: " + normalizedRoot);
        }

        List<ManifestFile> publicMods = new ArrayList<>();
        List<ManifestFile> publicOverrides = new ArrayList<>();
        List<PrivateFileEntry> privateFiles = new ArrayList<>();

        for (String scanDir : scanDirs) {
            walkDir(normalizedRoot, scanDir, publicMods, publicOverrides, privateFiles, known, unresolvedPolicy);
        }

        if (extraPrivatePaths != null) {
            for (String extra : extraPrivatePaths) {
                walkDir(normalizedRoot, extra, publicMods, publicOverrides, privateFiles, known, UnresolvedPolicy.PRIVATE);
            }
        }

        publicMods.sort((a, b) -> a.path.compareTo(b.path));
        publicOverrides.sort((a, b) -> a.path.compareTo(b.path));
        privateFiles.sort((a, b) -> a.path.compareTo(b.path));

        log.info("Manifest scan: {} public mods, {} public overrides, {} private from {}",
                publicMods.size(), publicOverrides.size(), privateFiles.size(), normalizedRoot);
        return new ScanResult(publicMods, publicOverrides, privateFiles);
    }

    private static void walkDir(
            Path normalizedRoot,
            String scanDir,
            List<ManifestFile> publicMods,
            List<ManifestFile> publicOverrides,
            List<PrivateFileEntry> privateFiles,
            KnownSourceRegistry known,
            UnresolvedPolicy unresolvedPolicy
    ) throws IOException {
        String sanitizedDir = PathSanitizer.sanitize(scanDir.replace('\\', '/'));
        Path dir = normalizedRoot.resolve(sanitizedDir).normalize();
        if (!dir.startsWith(normalizedRoot)) {
            throw new IOException("Scan path escapes root: " + scanDir);
        }
        if (!Files.isDirectory(dir)) {
            log.debug("Scan path missing, skipping: {}", dir);
            return;
        }

        Files.walkFileTree(dir, new SimpleFileVisitor<Path>() {
            @Override
            public FileVisitResult preVisitDirectory(Path d, BasicFileAttributes attrs) {
                String name = d.getFileName().toString();
                if (name.startsWith(".") || "vesta-private".equals(name)) {
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

                boolean platformKnown = known.isKnownPlatform(relative);
                if (platformKnown) {
                    ManifestFile entry = buildPublicEntry(file, known.get(relative));
                    entry.path = relative;
                    addPublic(relative, entry, publicMods, publicOverrides);
                } else if (unresolvedPolicy == UnresolvedPolicy.PRIVATE) {
                    privateFiles.add(buildPrivateEntry(file, relative));
                } else {
                    ManifestFile entry = buildPublicEntry(file, serverSource());
                    entry.path = relative;
                    addPublic(relative, entry, publicMods, publicOverrides);
                }
                return FileVisitResult.CONTINUE;
            }
        });
    }

    private static void addPublic(
            String relative,
            ManifestFile entry,
            List<ManifestFile> mods,
            List<ManifestFile> overrides
    ) {
        if (relative.startsWith("mods/")) {
            mods.add(entry);
        } else {
            overrides.add(entry);
        }
    }

    private static ManifestFile buildPublicEntry(Path file, FileSource source) throws IOException {
        ManifestFile entry = new ManifestFile();
        entry.sha256 = FileHasher.sha256(file);
        entry.sha1 = FileHasher.sha1(file);
        entry.size = Files.size(file);
        entry.source = source != null ? source : serverSource();
        return entry;
    }

    private static PrivateFileEntry buildPrivateEntry(Path file, String relative) throws IOException {
        PrivateFileEntry entry = new PrivateFileEntry();
        entry.path = relative;
        entry.sha256 = FileHasher.sha256(file);
        entry.sha1 = FileHasher.sha1(file);
        entry.size = Files.size(file);
        return entry;
    }

    private static FileSource serverSource() {
        FileSource source = new FileSource();
        source.type = "server";
        return source;
    }

    public static final class ScanResult {
        public final List<ManifestFile> mods;
        public final List<ManifestFile> overrides;
        public final List<PrivateFileEntry> privateFiles;

        public ScanResult(
                List<ManifestFile> mods,
                List<ManifestFile> overrides,
                List<PrivateFileEntry> privateFiles
        ) {
            this.mods = Collections.unmodifiableList(new ArrayList<>(mods));
            this.overrides = Collections.unmodifiableList(new ArrayList<>(overrides));
            this.privateFiles = Collections.unmodifiableList(new ArrayList<>(privateFiles));
        }
    }
}
