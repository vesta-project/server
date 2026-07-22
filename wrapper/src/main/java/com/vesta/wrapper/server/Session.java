package com.vesta.wrapper.server;

import com.vesta.sync.protocol.PushUpdateAvailable;

import java.util.Collections;
import java.util.Set;
import java.util.UUID;
import java.util.function.Consumer;

public final class Session {
    public final String id;
    public final long createdAt;
    public volatile long lastSeen;
    public volatile String currentVersion;
    public volatile String bearerTokenHash;
    public volatile String role;
    /** Feature flags the client announced in its TCP handshake. */
    public volatile Set<String> capabilities = Collections.emptySet();
    /** Per-session push listener (registered in {@code ServerState} on handshake). */
    public volatile Consumer<PushUpdateAvailable> packUpdatesListener;

    public Session() {
        this.id = UUID.randomUUID().toString();
        this.createdAt = System.currentTimeMillis();
        this.lastSeen = this.createdAt;
    }

    public boolean isExpired(long ttlMillis) {
        return System.currentTimeMillis() - lastSeen > ttlMillis;
    }

    public void touch() {
        this.lastSeen = System.currentTimeMillis();
    }
}
