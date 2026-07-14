package com.vesta.sync.auth;

import java.nio.charset.StandardCharsets;
import java.security.MessageDigest;
import java.security.SecureRandom;
import java.util.Base64;
import java.util.Objects;

/**
 * Issues and validates opaque bearer tokens. Password verification uses SHA-256 with a
 * per-server salt (sufficient for local operator passwords).
 */
public final class TokenService {

    private static final SecureRandom RANDOM = new SecureRandom();
    private static final char[] HEX = "0123456789abcdef".toCharArray();

    private final String passwordHashHex;
    private final String passwordSaltHex;

    public TokenService(String plaintextPassword) {
        if (plaintextPassword == null || plaintextPassword.trim().isEmpty()) {
            this.passwordSaltHex = null;
            this.passwordHashHex = null;
        } else {
            byte[] salt = new byte[16];
            RANDOM.nextBytes(salt);
            this.passwordSaltHex = toHex(salt);
            this.passwordHashHex = hashPassword(plaintextPassword, salt);
        }
    }

    public TokenService(String passwordSaltHex, String passwordHashHex) {
        this.passwordSaltHex = passwordSaltHex;
        this.passwordHashHex = passwordHashHex;
    }

    public boolean hasPassword() {
        return passwordHashHex != null;
    }

    public boolean verifyPassword(String plaintext) {
        if (passwordHashHex == null || plaintext == null) {
            return false;
        }
        byte[] salt = fromHex(passwordSaltHex);
        return MessageDigest.isEqual(
                passwordHashHex.getBytes(StandardCharsets.UTF_8),
                hashPassword(plaintext, salt).getBytes(StandardCharsets.UTF_8)
        );
    }

    public static String issueOpaqueToken() {
        byte[] bytes = new byte[32];
        RANDOM.nextBytes(bytes);
        return Base64.getUrlEncoder().withoutPadding().encodeToString(bytes);
    }

    public static String hashToken(String token) {
        Objects.requireNonNull(token, "token");
        try {
            MessageDigest md = MessageDigest.getInstance("SHA-256");
            return toHex(md.digest(token.getBytes(StandardCharsets.UTF_8)));
        } catch (Exception e) {
            throw new IllegalStateException("SHA-256 unavailable", e);
        }
    }

    private static String hashPassword(String password, byte[] salt) {
        try {
            MessageDigest md = MessageDigest.getInstance("SHA-256");
            md.update(salt);
            md.update(password.getBytes(StandardCharsets.UTF_8));
            return toHex(md.digest());
        } catch (Exception e) {
            throw new IllegalStateException("SHA-256 unavailable", e);
        }
    }

    static String toHex(byte[] bytes) {
        char[] out = new char[bytes.length * 2];
        for (int i = 0; i < bytes.length; i++) {
            int v = bytes[i] & 0xFF;
            out[i * 2] = HEX[v >>> 4];
            out[i * 2 + 1] = HEX[v & 0x0F];
        }
        return new String(out);
    }

    static byte[] fromHex(String hex) {
        int len = hex.length();
        byte[] out = new byte[len / 2];
        for (int i = 0; i < len; i += 2) {
            out[i / 2] = (byte) ((Character.digit(hex.charAt(i), 16) << 4)
                    + Character.digit(hex.charAt(i + 1), 16));
        }
        return out;
    }
}
