package com.vesta.wrapper.server;

import com.vesta.sync.hashing.FileHasher;
import com.vesta.sync.util.PathSanitizer;
import org.slf4j.Logger;
import org.slf4j.LoggerFactory;

import java.io.IOException;
import java.io.InputStream;
import java.nio.file.DirectoryStream;
import java.nio.file.Files;
import java.nio.file.Path;
import java.nio.file.StandardCopyOption;

public final class FileStore {

    private static final Logger log = LoggerFactory.getLogger(FileStore.class);
    private final Path root;

    public FileStore(Path root) throws IOException {
        this.root = root;
        Files.createDirectories(root);
    }

    public Path getRoot() {
        return root;
    }

    public Path resolveHash(String sha256) {
        String relativePath = PathSanitizer.hashPath(sha256);
        return root.resolve(relativePath);
    }

    public Path store(Path source) throws IOException {
        String hash = FileHasher.sha256(source);
        Path dest = resolveHash(hash);
        if (Files.exists(dest)) {
            log.debug("File already in store: {}", hash);
            return dest;
        }
        Files.createDirectories(dest.getParent());
        Files.copy(source, dest, StandardCopyOption.REPLACE_EXISTING);
        log.info("Stored: {} -> {}", source.getFileName(), hash);
        return dest;
    }

    public Path store(byte[] data) throws IOException {
        String hash = FileHasher.sha256(data);
        Path dest = resolveHash(hash);
        if (Files.exists(dest)) {
            return dest;
        }
        Files.createDirectories(dest.getParent());
        Files.write(dest, data);
        log.info("Stored bytes as: {}", hash);
        return dest;
    }

    public Path get(String sha256) {
        Path path = resolveHash(sha256);
        if (Files.exists(path) && !Files.isDirectory(path)) {
            return path;
        }
        return null;
    }

    public boolean exists(String sha256) {
        return get(sha256) != null;
    }

    public long size(String sha256) throws IOException {
        Path path = get(sha256);
        if (path == null) return 0;
        return Files.size(path);
    }

    public InputStream openStream(String sha256) throws IOException {
        Path path = get(sha256);
        if (path == null) throw new IOException("File not found: " + sha256);
        return Files.newInputStream(path);
    }

    public int count() throws IOException {
        int total = 0;
        try (DirectoryStream<Path> ds = Files.newDirectoryStream(root)) {
            for (Path sub : ds) {
                if (Files.isDirectory(sub)) {
                    try (DirectoryStream<Path> files = Files.newDirectoryStream(sub)) {
                        for (Path f : files) {
                            if (!Files.isDirectory(f)) total++;
                        }
                    }
                }
            }
        }
        return total;
    }
}
