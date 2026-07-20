package com.vesta.sync.auth;

import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.io.TempDir;

import java.nio.file.Files;
import java.nio.file.Path;
import java.time.Instant;
import java.util.UUID;

import static org.junit.jupiter.api.Assertions.*;

class BearerTokenPersistenceTest {

    @TempDir
    Path root;

    @Test
    void savesAndRestoresActiveTokensAndPurgesExpiredOnLoad() {
        BearerTokenStore original = new BearerTokenStore(3600);
        UUID uuid = UUID.randomUUID();
        String raw = original.issue("linked", uuid);
        String hash = TokenService.hashToken(raw);

        BearerTokenPersistence persistence = new BearerTokenPersistence(root);
        persistence.save(original);

        BearerTokenStore restored = new BearerTokenStore(3600);
        persistence.load(restored);
        assertTrue(restored.validateHash(hash).isPresent());
        assertEquals(uuid, restored.validateHash(hash).get().boundUuid);

        MutableClock clock = new MutableClock(Instant.parse("2026-01-01T00:00:00Z"));
        BearerTokenStore expiring = new BearerTokenStore(10, clock);
        expiring.issue("linked", uuid);
        persistence.save(expiring);
        clock.advance(java.time.Duration.ofSeconds(11));

        BearerTokenStore afterExpiry = new BearerTokenStore(3600);
        persistence.load(afterExpiry);
        assertEquals(0, afterExpiry.size());
        assertTrue(Files.isRegularFile(persistence.tokensFile()));
    }
}
