package com.vesta.sync.auth;

import org.junit.jupiter.api.Test;

import java.time.Duration;
import java.time.Instant;
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
                    assertFalse(serverId.trim().isEmpty());
                    return Optional.of(
                            new MinecraftSessionChallengeService.VerifiedProfile(profile, name)
                    );
                }
        );

        MinecraftSessionChallengeService.IssuedChallenge issued =
                service.issue(server, profile, "PlayerOne");
        assertTrue(service.complete(server, issued.challengeId()).isPresent());
        assertFalse(service.complete(server, issued.challengeId()).isPresent());
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

        MinecraftSessionChallengeService.IssuedChallenge issued =
                service.issue(server, profile, "PlayerOne");
        assertFalse(service.complete(UUID.randomUUID(), issued.challengeId()).isPresent());
        assertFalse(service.complete(server, issued.challengeId()).isPresent());
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

        MinecraftSessionChallengeService.IssuedChallenge issued =
                service.issue(server, UUID.randomUUID(), "PlayerOne");
        assertFalse(service.complete(server, issued.challengeId()).isPresent());
    }

    @Test
    void expiresWithoutCallingTheSessionVerifier() {
        MutableClock clock = new MutableClock(Instant.parse("2026-01-01T00:00:00Z"));
        AtomicInteger calls = new AtomicInteger();
        UUID server = UUID.randomUUID();
        MinecraftSessionChallengeService service = new MinecraftSessionChallengeService(
                Duration.ofSeconds(10),
                (name, serverId) -> {
                    calls.incrementAndGet();
                    return Optional.empty();
                },
                new java.security.SecureRandom(),
                clock
        );

        MinecraftSessionChallengeService.IssuedChallenge issued =
                service.issue(server, UUID.randomUUID(), "PlayerOne");
        clock.advance(Duration.ofSeconds(10));

        assertFalse(service.complete(server, issued.challengeId()).isPresent());
        assertEquals(0, calls.get());
    }

    @Test
    void rejectsInvalidProfileNamesBeforeIssuingAChallenge() {
        MinecraftSessionChallengeService service = new MinecraftSessionChallengeService(
                Duration.ofSeconds(60), (name, serverId) -> Optional.empty());

        assertThrows(IllegalArgumentException.class,
                () -> service.issue(UUID.randomUUID(), UUID.randomUUID(), "contains space"));
        assertThrows(IllegalArgumentException.class,
                () -> service.issue(UUID.randomUUID(), UUID.randomUUID(), ""));
        assertEquals(0, service.size());
    }

    @Test
    void rejectsMismatchedVerifiedNameAndConsumesAttempt() {
        UUID server = UUID.randomUUID();
        UUID profile = UUID.randomUUID();
        MinecraftSessionChallengeService service = new MinecraftSessionChallengeService(
                Duration.ofSeconds(60),
                (name, serverId) -> Optional.of(
                        new MinecraftSessionChallengeService.VerifiedProfile(profile, "SomeoneElse"))
        );

        MinecraftSessionChallengeService.IssuedChallenge issued =
                service.issue(server, profile, "PlayerOne");
        assertFalse(service.complete(server, issued.challengeId()).isPresent());
        assertFalse(service.complete(server, issued.challengeId()).isPresent());
    }
}
