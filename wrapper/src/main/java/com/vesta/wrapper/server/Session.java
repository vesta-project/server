package com.vesta.wrapper.server;

import java.util.UUID;

public final class Session {
    public final String id;
    public final long createdAt;
    public volatile long lastSeen;
    public volatile String currentVersion;

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
