package com.vesta.wrapper.identity;

import org.junit.jupiter.api.Test;
import org.junit.jupiter.params.ParameterizedTest;
import org.junit.jupiter.params.provider.Arguments;
import org.junit.jupiter.params.provider.MethodSource;

import java.util.ArrayList;
import java.util.Arrays;
import java.util.Base64;
import java.util.HashSet;
import java.util.concurrent.Executors;
import java.util.stream.Stream;

import static org.junit.jupiter.api.Assertions.*;

class PasswordHasherTest {
    private static final String PREFIX = "$argon2id$v=19$m=1024,t=1,p=1$";
    private static final String SALT = Base64.getEncoder().withoutPadding().encodeToString(new byte[16]);
    private static final String KEY = Base64.getEncoder().withoutPadding().encodeToString(new byte[32]);
    private static final String VALID_SHAPE = PREFIX + SALT + "$" + KEY;
    private static final PasswordHasher FAST = new PasswordHasher(PasswordHasher.HARD_MIN, PasswordHasher.DEFAULT_VERIFY_LIMIT);

    @Test
    void defaultHashUsesProductionPolicyFreshSaltAndConstantFormat() {
        PasswordHasher hasher = new PasswordHasher();
        char[] password = "public fixture password".toCharArray();
        char[] original = password.clone();
        String first = hasher.hash(password);
        String second = hasher.hash(password);
        assertTrue(first.startsWith("$argon2id$v=19$m=19456,t=2,p=1$"));
        assertNotEquals(first, second);
        String[] fields = first.split("\\$", -1);
        assertEquals(6, fields.length);
        assertEquals(16, Base64.getDecoder().decode(fields[4]).length);
        assertEquals(32, Base64.getDecoder().decode(fields[5]).length);
        assertFalse(first.contains("=" + "="));
        assertTrue(hasher.verify(password, first));
        assertFalse(hasher.verify("different password".toCharArray(), first));
        assertFalse(hasher.needsRehash(first));
        assertArrayEquals(original, password, "caller retains ownership of the input buffer");
        assertFalse(hasher.toString().contains(first));
        Arrays.fill(password, '\0');
    }

    @Test
    void verifiesIndependentBunArgon2idReferenceFixturesIncludingUtf8() {
        // Public fixed fixtures generated with Bun 1.3.14 password.hashSync, Argon2id,
        // memoryCost=19456, timeCost=2 on 2026-10-04. Bun and BouncyCastle are independent.
        PasswordHasher hasher = new PasswordHasher();
        String ascii = "$argon2id$v=19$m=19456,t=2,p=1$HK+ejNGt8MY161luKZ1r5ih0jPow7v2LLPfMx28uPNk$Z2hzM3WmcE55SafQUHKfAnXQ46iPtGH7+P7si2acXWk";
        String utf8 = "$argon2id$v=19$m=19456,t=2,p=1$xrDkR5kg45KHoAyegBqrAQJKrIN92DH9mPgEdQSwR1c$z0w/VLz8qh8B+B5iuiX9lHUnhXNZt/Alw8+Z20wVwds";
        assertTrue(hasher.verify("vesta-public-test-password".toCharArray(), ascii));
        assertTrue(hasher.verify("Vesta-🔐-café".toCharArray(), utf8));
        assertFalse(hasher.verify("vesta-public-test-password!".toCharArray(), ascii));
        assertTrue(hasher.needsRehash(ascii), "Bun's 32-byte salt differs from our new-hash policy");
    }

    @Test
    void weakOrDifferentlySizedStoredHashIsFlaggedWithoutDowngradingStrongerCosts() {
        PasswordHasher production = new PasswordHasher();
        assertTrue(production.needsRehash(FAST.hash("fixture".toCharArray())));
        String stronger = VALID_SHAPE.replace("m=1024,t=1,p=1", "m=32768,t=3,p=2");
        assertFalse(production.needsRehash(stronger));
        String mixed = VALID_SHAPE.replace("m=1024,t=1,p=1", "m=32768,t=1,p=1");
        assertTrue(production.needsRehash(mixed));
        String shorterKey = stronger.substring(0, stronger.lastIndexOf('$') + 1)
                + Base64.getEncoder().withoutPadding().encodeToString(new byte[16]);
        assertTrue(production.needsRehash(shorterKey));
    }

    @Test
    void maximumUtf8PasswordRoundTripsWithoutNormalizationOrTruncation() {
        char[] password = "漢".repeat(1024).toCharArray();
        String hash = FAST.hash(password);
        assertTrue(FAST.verify(password, hash));
        assertFalse(FAST.verify("漢".repeat(1023).toCharArray(), hash));
        String normalized = FAST.hash("café".toCharArray());
        assertFalse(FAST.verify("café".toCharArray(), normalized));
    }

    @ParameterizedTest
    @MethodSource("invalidPasswords")
    void rejectsInvalidPasswordsWithSecretFreeErrors(char[] password) {
        PasswordHashException error = assertThrows(PasswordHashException.class, () -> FAST.hash(password));
        assertEquals(PasswordHashException.INVALID_PASSWORD, error.getCode());
        assertEquals("password hash error: invalid_password", error.getMessage());
        assertNull(error.getCause());
        assertThrows(PasswordHashException.class, () -> FAST.verify(password, VALID_SHAPE));
    }

    static Stream<Arguments> invalidPasswords() {
        return Stream.of(Arguments.of((Object) null), Arguments.of((Object) new char[0]),
                Arguments.of((Object) "x".repeat(1025).toCharArray()),
                Arguments.of((Object) new char[] {'x', '\ud800'}),
                Arguments.of((Object) new char[] {'\udc00'}));
    }

    @ParameterizedTest
    @MethodSource("invalidHashes")
    void rejectsMalformedUnsupportedAndUnboundedHashesBeforeDerivation(String encoded, String code) {
        PasswordHashException error = assertThrows(PasswordHashException.class,
                () -> FAST.verify("public fixture".toCharArray(), encoded));
        assertEquals(code, error.getCode());
        assertEquals("password hash error: " + code, error.getMessage());
        assertNull(error.getCause());
        assertFalse(error.toString().contains("public fixture"));
        PasswordHashException rehash = assertThrows(PasswordHashException.class, () -> FAST.needsRehash(encoded));
        assertEquals(code, rehash.getCode());
    }

    static Stream<Arguments> invalidHashes() {
        String malformed = PasswordHashException.MALFORMED_HASH;
        String unsupported = PasswordHashException.UNSUPPORTED_ALGORITHM;
        String cost = PasswordHashException.COST_OUT_OF_BOUNDS;
        return Stream.of(
                Arguments.of(null, malformed), Arguments.of("", malformed),
                Arguments.of("plaintext-public-password", malformed),
                Arguments.of("x".repeat(257), malformed),
                Arguments.of(VALID_SHAPE + "$", malformed),
                Arguments.of(" " + VALID_SHAPE, malformed),
                Arguments.of(VALID_SHAPE + "\n", malformed),
                Arguments.of(VALID_SHAPE.replace("argon2id", "argon2i"), unsupported),
                Arguments.of(VALID_SHAPE.replace("v=19", "v=16"), unsupported),
                Arguments.of(VALID_SHAPE.replace("v=19", "v=019"), unsupported),
                Arguments.of(VALID_SHAPE.replace("m=1024", "m=01024"), malformed),
                Arguments.of(VALID_SHAPE.replace("m=1024", "m=+1024"), malformed),
                Arguments.of(VALID_SHAPE.replace("m=1024", "m=-1024"), malformed),
                Arguments.of(VALID_SHAPE.replace("m=1024", "m=9999999999"), malformed),
                Arguments.of(VALID_SHAPE.replace("m=1024", "m=1e6"), malformed),
                Arguments.of(VALID_SHAPE.replace("m=1024,t=1,p=1", "t=1,m=1024,p=1"), malformed),
                Arguments.of(VALID_SHAPE.replace("m=1024,t=1,p=1", "m=1024,t=1,t=1"), malformed),
                Arguments.of(VALID_SHAPE.replace("m=1024,t=1,p=1", "m=1024,t=1,p=1,x=2"), malformed),
                Arguments.of(VALID_SHAPE.replace("m=1024", "m=0"), cost),
                Arguments.of(VALID_SHAPE.replace("m=1024", "m=65537"), cost),
                Arguments.of(VALID_SHAPE.replace("m=1024", "m=999999999"), cost),
                Arguments.of(VALID_SHAPE.replace("t=1", "t=0"), cost),
                Arguments.of(VALID_SHAPE.replace("t=1", "t=5"), cost),
                Arguments.of(VALID_SHAPE.replace("p=1", "p=0"), cost),
                Arguments.of(VALID_SHAPE.replace("p=1", "p=5"), cost),
                Arguments.of(PREFIX + SALT + "=$" + KEY, malformed),
                Arguments.of(PREFIX + SALT.substring(0, SALT.length() - 1) + "B$" + KEY, malformed),
                Arguments.of(PREFIX + "AA$" + KEY, malformed),
                Arguments.of(PREFIX + "A".repeat(88) + "$" + KEY, malformed),
                Arguments.of(PREFIX + SALT + "$AA", malformed),
                Arguments.of(PREFIX + SALT + "$" + "A".repeat(88), malformed),
                Arguments.of(PREFIX + SALT + "$" + KEY.replace('A', '_'), malformed));
    }

    @Test
    void configuredPoliciesHaveHardBoundsAndCannotHashAboveTheirVerificationLimit() {
        for (int[] values : new int[][] {{0, 1, 1}, {1023, 1, 1}, {262145, 1, 1},
                {1024, 0, 1}, {1024, 17, 1}, {1024, 1, 0}, {1024, 1, 9}}) {
            assertEquals(PasswordHashException.INVALID_POLICY, assertThrows(PasswordHashException.class,
                    () -> new PasswordHasher.CostPolicy(values[0], values[1], values[2])).getCode());
        }
        assertThrows(PasswordHashException.class,
                () -> new PasswordHasher(PasswordHasher.DEFAULT_COST, PasswordHasher.HARD_MIN));
        assertEquals(PasswordHasher.DEFAULT_COST, new PasswordHasher().cost());
        assertEquals(PasswordHasher.DEFAULT_VERIFY_LIMIT, new PasswordHasher().verifyLimit());
        assertTrue(PasswordHasher.HARD_MIN.within(PasswordHasher.DEFAULT_COST));
        assertTrue(PasswordHasher.DEFAULT_COST.atLeast(PasswordHasher.HARD_MIN));
    }

    @Test
    void oneHasherCanBeSharedAcrossConcurrentCallersWithDistinctSalts() throws Exception {
        var pool = Executors.newFixedThreadPool(4);
        try {
            var futures = new ArrayList<java.util.concurrent.Future<String>>();
            for (int i = 0; i < 12; i++) {
                futures.add(pool.submit(() -> {
                    String hash = FAST.hash("concurrent fixture".toCharArray());
                    assertTrue(FAST.verify("concurrent fixture".toCharArray(), hash));
                    return hash;
                }));
            }
            var unique = new HashSet<String>();
            for (var future : futures) unique.add(future.get(10, java.util.concurrent.TimeUnit.SECONDS));
            assertEquals(12, unique.size());
        } finally {
            pool.shutdownNow();
            assertTrue(pool.awaitTermination(10, java.util.concurrent.TimeUnit.SECONDS));
        }
    }
}
