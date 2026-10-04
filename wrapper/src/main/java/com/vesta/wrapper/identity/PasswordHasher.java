package com.vesta.wrapper.identity;

import org.bouncycastle.crypto.generators.Argon2BytesGenerator;
import org.bouncycastle.crypto.params.Argon2Parameters;

import java.nio.ByteBuffer;
import java.nio.CharBuffer;
import java.nio.charset.CoderResult;
import java.nio.charset.CharsetEncoder;
import java.nio.charset.CodingErrorAction;
import java.nio.charset.StandardCharsets;
import java.security.SecureRandom;
import java.util.Arrays;
import java.util.Base64;
import java.util.Objects;

/**
 * Argon2id hashing/verification primitive for human passwords (RFC 9106 Argon2id, version 0x13).
 *
 * <p>This is only the cryptographic primitive: it is not a login endpoint, credential store, vault or
 * session mechanism, and it does not rate-limit. Callers must bound attempt rate and concurrency.
 *
 * <p>Encoded form (PHC string, standard Base64 alphabet without padding):
 * <pre>$argon2id$v=19$m=&lt;KiB&gt;,t=&lt;iterations&gt;,p=&lt;lanes&gt;$&lt;salt&gt;$&lt;hash&gt;</pre>
 * Parsing is strict and canonical: exact field order, no duplicates or extra/trailing fields, decimal
 * numbers without sign or leading zeros, canonical Base64 only. Every length and cost parameter is checked
 * against this hasher's {@linkplain #verifyLimit() verification limit} BEFORE key derivation or any
 * large cost-dependent allocation (bounded parsing itself allocates only small, length-capped buffers),
 * so attacker-supplied hashes cannot force large memory/CPU work. There is no legacy,
 * plaintext or alternative-algorithm fallback, and empty passwords are always rejected.
 *
 * <p>Instances are immutable and thread-safe. Errors are {@link PasswordHashException}s carrying only a
 * fixed code; no password, salt or hash material ever appears in messages or {@code toString()}.
 */
public final class PasswordHasher {
    public static final String ALGORITHM = "argon2id";
    public static final int VERSION = Argon2Parameters.ARGON2_VERSION_13; // 0x13 = 19
    public static final int SALT_BYTES = 16;
    public static final int HASH_BYTES = 32;
    public static final int MIN_SALT_BYTES = 16;
    public static final int MAX_SALT_BYTES = 64;
    public static final int MIN_HASH_BYTES = 16;
    public static final int MAX_HASH_BYTES = 64;
    /** Maximum password length in chars and in UTF-8 bytes. */
    public static final int MAX_PASSWORD_CHARS = 1024;
    public static final int MAX_PASSWORD_BYTES = 4096;
    /** Upper bound on an encoded hash string, checked before any parsing. */
    public static final int MAX_ENCODED_LENGTH = 256;
    private static final int MAX_DECIMAL_DIGITS = 9;

    static final int MIN_MEMORY_KIB = 1024;
    static final int MAX_MEMORY_KIB = 262_144;
    static final int MIN_ITERATIONS = 1;
    static final int MAX_ITERATIONS = 16;
    static final int MIN_PARALLELISM = 1;
    static final int MAX_PARALLELISM = 8;

    /** Absolute floor for any policy (also the floor accepted at verification). Test-speed, not production. */
    public static final CostPolicy HARD_MIN = new CostPolicy(MIN_MEMORY_KIB, MIN_ITERATIONS, MIN_PARALLELISM);
    /** Absolute ceiling for any policy or verification limit. */
    public static final CostPolicy HARD_MAX = new CostPolicy(MAX_MEMORY_KIB, MAX_ITERATIONS, MAX_PARALLELISM);
    /** Production default: OWASP Argon2id baseline (19 MiB, 2 passes, 1 lane). */
    public static final CostPolicy DEFAULT_COST = new CostPolicy(19_456, 2, 1);
    /** Default ceiling for parameters accepted from stored hashes (64 MiB, 4 passes, 4 lanes). */
    public static final CostPolicy DEFAULT_VERIFY_LIMIT = new CostPolicy(65_536, 4, 4);

    /**
     * Argon2id cost: memory in KiB, iterations (passes) and parallelism (lanes). Validated against
     * {@link #HARD_MIN}/{@link #HARD_MAX} and Argon2's {@code memory >= 8 * lanes} rule.
     */
    public record CostPolicy(int memoryKiB, int iterations, int parallelism) {
        public CostPolicy {
            if (memoryKiB < MIN_MEMORY_KIB || memoryKiB > MAX_MEMORY_KIB
                    || iterations < MIN_ITERATIONS || iterations > MAX_ITERATIONS
                    || parallelism < MIN_PARALLELISM || parallelism > MAX_PARALLELISM
                    || memoryKiB < 8 * parallelism) {
                throw new PasswordHashException(PasswordHashException.INVALID_POLICY);
            }
        }

        /** True when every component of this policy is within {@code limit}. */
        public boolean within(CostPolicy limit) {
            return memoryKiB <= limit.memoryKiB && iterations <= limit.iterations && parallelism <= limit.parallelism;
        }

        /** True when every component is at least {@code other}'s. */
        public boolean atLeast(CostPolicy other) {
            return memoryKiB >= other.memoryKiB && iterations >= other.iterations && parallelism >= other.parallelism;
        }
    }

    private final CostPolicy cost;
    private final CostPolicy verifyLimit;
    private final SecureRandom random;

    /** Production defaults: {@link #DEFAULT_COST} hashing, {@link #DEFAULT_VERIFY_LIMIT} verification. */
    public PasswordHasher() {
        this(DEFAULT_COST, DEFAULT_VERIFY_LIMIT);
    }

    /**
     * @param cost        cost used for new hashes
     * @param verifyLimit largest stored parameters accepted by {@link #verify}; must be at least {@code cost}
     */
    public PasswordHasher(CostPolicy cost, CostPolicy verifyLimit) {
        this.cost = Objects.requireNonNull(cost, "cost");
        this.verifyLimit = Objects.requireNonNull(verifyLimit, "verifyLimit");
        if (!cost.within(verifyLimit)) {
            throw new PasswordHashException(PasswordHashException.INVALID_POLICY);
        }
        this.random = new SecureRandom();
    }

    public CostPolicy cost() {
        return cost;
    }

    public CostPolicy verifyLimit() {
        return verifyLimit;
    }

    /**
     * Hashes {@code password} with a fresh random salt. The caller keeps ownership of the array and should
     * clear it afterwards; internal copies are cleared before return.
     */
    public String hash(char[] password) {
        byte[] salt = new byte[SALT_BYTES];
        byte[] derived = new byte[HASH_BYTES];
        byte[] passwordBytes = encodePassword(password);
        try {
            random.nextBytes(salt);
            derive(passwordBytes, salt, cost, derived);
            Base64.Encoder b64 = Base64.getEncoder().withoutPadding();
            return "$" + ALGORITHM + "$v=" + VERSION + "$m=" + cost.memoryKiB() + ",t=" + cost.iterations()
                    + ",p=" + cost.parallelism() + "$" + b64.encodeToString(salt) + "$" + b64.encodeToString(derived);
        } finally {
            Arrays.fill(passwordBytes, (byte) 0);
            Arrays.fill(derived, (byte) 0);
        }
    }

    /**
     * Verifies {@code password} against an encoded hash. Returns {@code false} only for a well-formed hash
     * that does not match. Malformed, unsupported or out-of-limit hashes and invalid passwords throw
     * {@link PasswordHashException} before key derivation or any cost-dependent allocation.
     */
    public boolean verify(char[] password, String encoded) {
        Parsed parsed = parse(encoded);                 // all bounds checked, no KDF yet
        byte[] passwordBytes = encodePassword(password); // bounded and validated before KDF
        byte[] derived = new byte[parsed.hash.length];
        try {
            derive(passwordBytes, parsed.salt, parsed.cost, derived);
            return org.bouncycastle.util.Arrays.constantTimeAreEqual(derived, parsed.hash);
        } finally {
            Arrays.fill(passwordBytes, (byte) 0);
            Arrays.fill(derived, (byte) 0);
            Arrays.fill(parsed.hash, (byte) 0);
        }
    }

    /**
     * True when a well-formed hash should be replaced after a successful verify: its cost is below this
     * hasher's {@link #cost()} or its salt/hash sizes differ from current defaults. Throws like
     * {@link #verify} for malformed or out-of-limit input. Performs no key derivation.
     */
    public boolean needsRehash(String encoded) {
        Parsed parsed = parse(encoded);
        Arrays.fill(parsed.hash, (byte) 0);
        return !parsed.cost.atLeast(cost) || parsed.salt.length != SALT_BYTES || parsed.hash.length != HASH_BYTES;
    }

    @Override
    public String toString() {
        return "PasswordHasher[argon2id, cost=" + cost + ", verifyLimit=" + verifyLimit + "]";
    }

    // ------------------------------------------------------------------ internals

    private record Parsed(CostPolicy cost, byte[] salt, byte[] hash) {
        @Override
        public String toString() {
            return "Parsed[cost=" + cost + ", salt=<redacted>, hash=<redacted>]";
        }
    }

    private Parsed parse(String encoded) {
        if (encoded == null || encoded.isEmpty() || encoded.length() > MAX_ENCODED_LENGTH) {
            throw new PasswordHashException(PasswordHashException.MALFORMED_HASH);
        }
        for (int i = 0; i < encoded.length(); i++) {
            char c = encoded.charAt(i);
            if (c < 0x21 || c > 0x7e) {
                throw new PasswordHashException(PasswordHashException.MALFORMED_HASH);
            }
        }
        String[] fields = encoded.split("\\$", -1);
        // "", algorithm, version, params, salt, hash — exactly; trailing/extra/missing fields rejected.
        if (fields.length != 6 || !fields[0].isEmpty()) {
            throw new PasswordHashException(PasswordHashException.MALFORMED_HASH);
        }
        if (!ALGORITHM.equals(fields[1])) {
            throw new PasswordHashException(PasswordHashException.UNSUPPORTED_ALGORITHM);
        }
        if (!("v=" + VERSION).equals(fields[2])) {
            throw new PasswordHashException(PasswordHashException.UNSUPPORTED_ALGORITHM);
        }
        String[] params = fields[3].split(",", -1);
        if (params.length != 3) {
            throw new PasswordHashException(PasswordHashException.MALFORMED_HASH);
        }
        int m = decimal(params[0], "m=");
        int t = decimal(params[1], "t=");
        int p = decimal(params[2], "p=");
        if (m < HARD_MIN.memoryKiB() || t < HARD_MIN.iterations() || p < HARD_MIN.parallelism()
                || m > verifyLimit.memoryKiB() || t > verifyLimit.iterations() || p > verifyLimit.parallelism()
                || m < 8 * p) {
            throw new PasswordHashException(PasswordHashException.COST_OUT_OF_BOUNDS);
        }
        byte[] salt = base64(fields[4], MIN_SALT_BYTES, MAX_SALT_BYTES);
        byte[] hash = base64(fields[5], MIN_HASH_BYTES, MAX_HASH_BYTES);
        return new Parsed(new CostPolicy(m, t, p), salt, hash);
    }

    /** Parses {@code prefix}{digits}: no sign, no leading zero, bounded digit count. */
    private static int decimal(String field, String prefix) {
        if (!field.startsWith(prefix)) {
            throw new PasswordHashException(PasswordHashException.MALFORMED_HASH);
        }
        String digits = field.substring(prefix.length());
        if (digits.isEmpty() || digits.length() > MAX_DECIMAL_DIGITS
                || (digits.length() > 1 && digits.charAt(0) == '0')) {
            throw new PasswordHashException(PasswordHashException.MALFORMED_HASH);
        }
        int value = 0;
        for (int i = 0; i < digits.length(); i++) {
            char c = digits.charAt(i);
            if (c < '0' || c > '9') {
                throw new PasswordHashException(PasswordHashException.MALFORMED_HASH);
            }
            value = value * 10 + (c - '0');
        }
        return value;
    }

    /** Canonical unpadded standard Base64 with a decoded length in [min, max]; checked before decoding. */
    private static byte[] base64(String text, int minBytes, int maxBytes) {
        int len = text.length();
        // Unpadded Base64 length for n bytes is ceil(4n/3); length % 4 == 1 is never valid.
        if (len == 0 || len % 4 == 1 || len > (4 * maxBytes + 2) / 3 || len < (4 * minBytes + 2) / 3) {
            throw new PasswordHashException(PasswordHashException.MALFORMED_HASH);
        }
        for (int i = 0; i < len; i++) {
            char c = text.charAt(i);
            boolean ok = (c >= 'A' && c <= 'Z') || (c >= 'a' && c <= 'z') || (c >= '0' && c <= '9') || c == '+' || c == '/';
            if (!ok) {
                throw new PasswordHashException(PasswordHashException.MALFORMED_HASH);
            }
        }
        byte[] out;
        try {
            out = Base64.getDecoder().decode(text);
        } catch (IllegalArgumentException e) {
            throw new PasswordHashException(PasswordHashException.MALFORMED_HASH);
        }
        // Reject non-canonical encodings (non-zero unused trailing bits) by round-tripping.
        if (out.length < minBytes || out.length > maxBytes
                || !Base64.getEncoder().withoutPadding().encodeToString(out).equals(text)) {
            Arrays.fill(out, (byte) 0);
            throw new PasswordHashException(PasswordHashException.MALFORMED_HASH);
        }
        return out;
    }

    /** Strict UTF-8 encoding of a bounded, non-empty password without creating a String. */
    private static byte[] encodePassword(char[] password) {
        if (password == null || password.length == 0 || password.length > MAX_PASSWORD_CHARS) {
            throw new PasswordHashException(PasswordHashException.INVALID_PASSWORD);
        }
        CharsetEncoder encoder = StandardCharsets.UTF_8.newEncoder()
                .onMalformedInput(CodingErrorAction.REPORT)
                .onUnmappableCharacter(CodingErrorAction.REPORT);
        // Explicitly owned, fixed-size buffer: the encoder never allocates its own secret buffer, and the
        // whole array is cleared on every exit (success, malformed input, overflow or unexpected error).
        byte[] backing = new byte[MAX_PASSWORD_BYTES];
        ByteBuffer buffer = ByteBuffer.wrap(backing);
        try {
            CoderResult result = encoder.encode(CharBuffer.wrap(password), buffer, true);
            if (!result.isUnderflow()) {
                throw new PasswordHashException(PasswordHashException.INVALID_PASSWORD); // malformed/overflow
            }
            result = encoder.flush(buffer);
            if (!result.isUnderflow()) {
                throw new PasswordHashException(PasswordHashException.INVALID_PASSWORD);
            }
            return Arrays.copyOf(backing, buffer.position());
        } finally {
            Arrays.fill(backing, (byte) 0);
        }
    }

    private static void derive(byte[] password, byte[] salt, CostPolicy cost, byte[] out) {
        Argon2Parameters params = new Argon2Parameters.Builder(Argon2Parameters.ARGON2_id)
                .withVersion(VERSION)
                .withMemoryAsKB(cost.memoryKiB())
                .withIterations(cost.iterations())
                .withParallelism(cost.parallelism())
                .withSalt(salt)
                .build();
        try {
            Argon2BytesGenerator generator = new Argon2BytesGenerator();
            generator.init(params);
            generator.generateBytes(password, out);
        } finally {
            params.clear();
        }
    }
}
