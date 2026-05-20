package com.vesta.sync.util;

public final class PathSanitizer {

    public static String sanitize(String path) {
        if (path == null || path.isEmpty()) {
            throw new IllegalArgumentException("Path cannot be null or empty");
        }

        String normalized = path.replace('\\', '/');

        if (normalized.startsWith("/")) {
            throw new IllegalArgumentException("Absolute paths not allowed: " + path);
        }

        String[] parts = normalized.split("/");
        for (String part : parts) {
            if (part.equals("..")) {
                throw new IllegalArgumentException("Path traversal detected: " + path);
            }
            if (part.isEmpty() && !parts[parts.length - 1].isEmpty()) {
                throw new IllegalArgumentException("Invalid path containing empty segment: " + path);
            }
            if (part.contains(":")) {
                throw new IllegalArgumentException("Invalid character in path: " + path);
            }
        }

        return normalized;
    }

    public static String hashPath(String sha256) {
        String prefix = sha256.substring(0, 2);
        String rest = sha256.substring(2);
        return sanitize(prefix + "/" + rest);
    }

    private PathSanitizer() {}
}
