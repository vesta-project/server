package com.vesta.sync.hashing;

import java.io.IOException;
import java.io.InputStream;
import java.nio.file.Files;
import java.nio.file.Path;
import java.security.MessageDigest;
import java.security.NoSuchAlgorithmException;

public final class FileHasher {

    private static final String SHA256 = "SHA-256";
    private static final String SHA1 = "SHA-1";
    private static final int BUFFER_SIZE = 8192;

    public static String sha1(Path file) throws IOException {
        return digestFile(file, SHA1);
    }

    public static String sha256(Path file) throws IOException {
        return digestFile(file, SHA256);
    }

    private static String digestFile(Path file, String algorithm) throws IOException {
        try {
            MessageDigest md = MessageDigest.getInstance(algorithm);
            byte[] buf = new byte[BUFFER_SIZE];
            try (InputStream in = Files.newInputStream(file)) {
                int read;
                while ((read = in.read(buf)) >= 0) {
                    md.update(buf, 0, read);
                }
            }
            return bytesToHex(md.digest());
        } catch (NoSuchAlgorithmException e) {
            throw new RuntimeException(algorithm + " not available", e);
        }
    }

    public static String sha256(byte[] data) {
        try {
            MessageDigest md = MessageDigest.getInstance(SHA256);
            return bytesToHex(md.digest(data));
        } catch (NoSuchAlgorithmException e) {
            throw new RuntimeException("SHA-256 not available", e);
        }
    }

    public static boolean verify(Path file, String expected) throws IOException {
        return sha256(file).equalsIgnoreCase(expected);
    }

    private static String bytesToHex(byte[] bytes) {
        StringBuilder sb = new StringBuilder(bytes.length * 2);
        for (byte b : bytes) {
            sb.append(String.format("%02x", b));
        }
        return sb.toString();
    }

    private FileHasher() {}
}
