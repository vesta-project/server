package com.vesta.sync.auth;

import org.junit.jupiter.api.Test;

import java.time.Duration;
import java.util.Optional;
import java.util.UUID;
import java.util.concurrent.atomic.AtomicInteger;

import static org.junit.jupiter.api.Assertions.*;

class MinecraftSessionChallengeServiceTest {

    @Test
    void completesOnceForTheBoundServerAndProfile() {
        UUID server = UUID.randomUUID();
        UUID profile = UUID.randomUUID();
        AtomicInteger calls = new AtomicInteger();
        MinecraftSessionChallengeService service = new MinecraftSessionChallengeService(
                Duration.ofSeconds(60),
                (name, serverId) -> {
                    calls.incrementAndGet();
                    assertEquals("PlayerOne", name);
                    assertFalse(serverId.isBlank());
                    return Optional.of(
                            new MinecraftSessionChallengeService.VerifiedProfile(profile, name)
                    );
                }
        );

        var issued = service.issue(server, profile, "PlayerOne");
        assertTrue(service.complete(server, issued.challengeId()).isPresent());
        assertTrue(service.complete(server, issued.challengeId()).isEmpty());
        assertEquals(1, calls.get());
    }

    @Test
    void rejectsWrongServerAndConsumesAttempt() {
        UUID server = UUID.randomUUID();
        UUID profile = UUID.randomUUID();
        MinecraftSessionChallengeService service = new MinecraftSessionChallengeService(
                Duration.ofSeconds(60),
                (name, serverId) -> Optional.of(
                        new MinecraftSessionChallengeService.VerifiedProfile(profile, name)
                )
        );

        var issued = service.issue(server, profile, "PlayerOne");
        assertTrue(service.complete(UUID.randomUUID(), issued.challengeId()).isEmpty());
        assertTrue(service.complete(server, issued.challengeId()).isEmpty());
    }

    @Test
    void rejectsMismatchedVerifiedProfile() {
        UUID server = UUID.randomUUID();
        MinecraftSessionChallengeService service = new MinecraftSessionChallengeService(
                Duration.ofSeconds(60),
                (name, serverId) -> Optional.of(
                        new MinecraftSessionChallengeService.VerifiedProfile(UUID.randomUUID(), name)
                )
        );

        var issued = service.issue(server, UUID.randomUUID(), "PlayerOne");
        assertTrue(service.complete(server, issued.challengeId()).isEmpty());
    }
}
