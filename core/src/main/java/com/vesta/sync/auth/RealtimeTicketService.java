package com.vesta.sync.auth;

import java.time.Clock;
import java.time.Duration;
import java.time.Instant;
import java.util.Collections;
import java.util.HashSet;
import java.util.Optional;
import java.util.Set;
import java.util.UUID;
import java.util.concurrent.ConcurrentHashMap;

/** Issues short-lived, single-use tickets for realtime WebSocket authentication. */
public final class RealtimeTicketService {

    public static final class Ticket {
        public final UUID serverUuid;
        public final UUID accountUuid;
        public final String bearerTokenHash;
        public final String role;
        public final Set<String> capabilities;
        public final Instant ticketExpiresAt;
        public final Instant sessionExpiresAt;

        private Ticket(UUID serverUuid, UUID accountUuid, String bearerTokenHash,
                       String role, Set<String> capabilities,
                       Instant ticketExpiresAt, Instant sessionExpiresAt) {
            this.serverUuid = serverUuid;
            this.accountUuid = accountUuid;
            this.bearerTokenHash = bearerTokenHash;
            this.role = role;
            this.capabilities = Collections.unmodifiableSet(new HashSet<>(capabilities));
            this.ticketExpiresAt = ticketExpiresAt;
            this.sessionExpiresAt = sessionExpiresAt;
        }
    }

    public static final class Issued {
        public final String value;
        public final Instant expiresAt;

        private Issued(String value, Instant expiresAt) {
            this.value = value;
            this.expiresAt = expiresAt;
        }
    }

    private final ConcurrentHashMap<String, Ticket> tickets = new ConcurrentHashMap<>();
    private final Duration ttl;
    private final Clock clock;

    public RealtimeTicketService(Duration ttl) {
        this(ttl, Clock.systemUTC());
    }

    RealtimeTicketService(Duration ttl, Clock clock) {
        this.ttl = ttl;
        this.clock = java.util.Objects.requireNonNull(clock, "clock");
    }

    public Issued issue(UUID serverUuid, BearerTokenStore.Record bearer, Set<String> capabilities) {
        if (serverUuid == null || bearer == null) {
            throw new IllegalArgumentException("server and bearer are required");
        }
        String raw = TokenService.issueOpaqueToken();
        Instant expiresAt = clock.instant().plus(ttl);
        tickets.put(TokenService.hashToken(raw), new Ticket(
                serverUuid,
                bearer.boundUuid,
                bearer.tokenHash,
                bearer.role,
                capabilities == null ? Collections.emptySet() : capabilities,
                expiresAt,
                bearer.expiresAt
        ));
        purgeExpired();
        return new Issued(raw, expiresAt);
    }

    /** Every authentication attempt consumes the ticket, including invalid ones. */
    public Optional<Ticket> consume(UUID serverUuid, String raw) {
        if (serverUuid == null || raw == null || raw.trim().isEmpty()) {
            return Optional.empty();
        }
        Ticket ticket = tickets.remove(TokenService.hashToken(raw.trim()));
        Instant now = clock.instant();
        if (ticket == null
                || !serverUuid.equals(ticket.serverUuid)
                || !now.isBefore(ticket.ticketExpiresAt)
                || !now.isBefore(ticket.sessionExpiresAt)) {
            return Optional.empty();
        }
        return Optional.of(ticket);
    }

    public void purgeExpired() {
        Instant now = clock.instant();
        tickets.entrySet().removeIf(entry -> !now.isBefore(entry.getValue().ticketExpiresAt));
    }

    public int size() {
        purgeExpired();
        return tickets.size();
    }
}
