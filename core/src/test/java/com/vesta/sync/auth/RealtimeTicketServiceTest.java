package com.vesta.sync.auth;

import org.junit.jupiter.api.Test;

import java.time.Duration;
import java.time.Instant;
import java.util.Arrays;
import java.util.Collections;
import java.util.HashSet;
import java.util.Set;
import java.util.UUID;

import static org.junit.jupiter.api.Assertions.*;

class RealtimeTicketServiceTest {

    @Test
    void consumesOnceAndPreservesBoundSessionData() {
        MutableClock clock = new MutableClock(Instant.parse("2026-01-01T00:00:00Z"));
        UUID server = UUID.randomUUID();
        UUID account = UUID.randomUUID();
        BearerTokenStore.Record bearer = bearer(account, clock.instant().plusSeconds(300));
        RealtimeTicketService service = new RealtimeTicketService(Duration.ofSeconds(30), clock);

        RealtimeTicketService.Issued issued = service.issue(
                server, bearer, capabilities());
        RealtimeTicketService.Ticket consumed = service.consume(server, issued.value)
                .orElseThrow(() -> new AssertionError("ticket was not accepted"));

        assertEquals(account, consumed.accountUuid);
        assertEquals(bearer.tokenHash, consumed.bearerTokenHash);
        assertEquals(capabilities(), consumed.capabilities);
        assertFalse(service.consume(server, issued.value).isPresent());
    }

    @Test
    void wrongServerConsumesTicket() {
        MutableClock clock = new MutableClock(Instant.parse("2026-01-01T00:00:00Z"));
        UUID server = UUID.randomUUID();
        RealtimeTicketService service = new RealtimeTicketService(Duration.ofSeconds(30), clock);
        String ticket = service.issue(
                server, bearer(UUID.randomUUID(), clock.instant().plusSeconds(300)),
                Collections.emptySet()).value;

        assertFalse(service.consume(UUID.randomUUID(), ticket).isPresent());
        assertFalse(service.consume(server, ticket).isPresent());
    }

    @Test
    void rejectsExpiredTicketAndExpiredBearerSession() {
        MutableClock clock = new MutableClock(Instant.parse("2026-01-01T00:00:00Z"));
        UUID server = UUID.randomUUID();
        RealtimeTicketService service = new RealtimeTicketService(Duration.ofSeconds(30), clock);
        String ticket = service.issue(
                server, bearer(UUID.randomUUID(), clock.instant().plusSeconds(300)),
                Collections.emptySet()).value;
        clock.advance(Duration.ofSeconds(30));
        assertFalse(service.consume(server, ticket).isPresent());

        String expiredSession = service.issue(
                server, bearer(UUID.randomUUID(), clock.instant()), Collections.emptySet()).value;
        assertFalse(service.consume(server, expiredSession).isPresent());
    }

    @Test
    void purgesExpiredTickets() {
        MutableClock clock = new MutableClock(Instant.parse("2026-01-01T00:00:00Z"));
        RealtimeTicketService service = new RealtimeTicketService(Duration.ofSeconds(5), clock);
        service.issue(UUID.randomUUID(),
                bearer(UUID.randomUUID(), clock.instant().plusSeconds(300)),
                Collections.emptySet());
        assertEquals(1, service.size());
        clock.advance(Duration.ofSeconds(5));
        assertEquals(0, service.size());
    }

    private static BearerTokenStore.Record bearer(UUID account, Instant expiresAt) {
        return new BearerTokenStore.Record(
                "bearer-hash", "linked", account, expiresAt.minusSeconds(60), expiresAt);
    }

    private static Set<String> capabilities() {
        return new HashSet<>(Arrays.asList("server_status", "pack_updates"));
    }
}
