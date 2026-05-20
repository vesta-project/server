package com.vesta.wrapper.server;

import org.slf4j.Logger;
import org.slf4j.LoggerFactory;

import java.util.concurrent.ConcurrentHashMap;
import java.util.concurrent.Executors;
import java.util.concurrent.ScheduledExecutorService;
import java.util.concurrent.TimeUnit;

public final class SessionManager {

    private static final Logger log = LoggerFactory.getLogger(SessionManager.class);
    private static final long TTL_MILLIS = 90_000;
    private static final long CLEANUP_INTERVAL_SEC = 30;

    private final ConcurrentHashMap<String, Session> sessions = new ConcurrentHashMap<>();
    private final ScheduledExecutorService scheduler = Executors.newSingleThreadScheduledExecutor(r -> {
        Thread t = new Thread(r, "vesta-session-cleanup");
        t.setDaemon(true);
        return t;
    });

    public SessionManager() {
        scheduler.scheduleAtFixedRate(this::cleanup, CLEANUP_INTERVAL_SEC, CLEANUP_INTERVAL_SEC, TimeUnit.SECONDS);
    }

    public Session create() {
        Session s = new Session();
        sessions.put(s.id, s);
        log.info("Session created: {}", s.id);
        return s;
    }

    public Session get(String id) {
        Session s = sessions.get(id);
        if (s != null && s.isExpired(TTL_MILLIS)) {
            sessions.remove(id);
            log.info("Session expired: {}", id);
            return null;
        }
        return s;
    }

    public void remove(String id) {
        sessions.remove(id);
        log.info("Session removed: {}", id);
    }

    public void touch(String id) {
        Session s = sessions.get(id);
        if (s != null) {
            s.touch();
        }
    }

    public int count() {
        return sessions.size();
    }

    private void cleanup() {
        int before = sessions.size();
        sessions.entrySet().removeIf(e -> {
            if (e.getValue().isExpired(TTL_MILLIS)) {
                log.info("Cleaned up expired session: {}", e.getKey());
                return true;
            }
            return false;
        });
        int removed = before - sessions.size();
        if (removed > 0) {
            log.debug("Session cleanup removed {} sessions, {} remaining", removed, sessions.size());
        }
    }

    public void shutdown() {
        scheduler.shutdown();
        sessions.clear();
    }
}
