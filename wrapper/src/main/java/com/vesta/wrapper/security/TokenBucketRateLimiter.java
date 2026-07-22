package com.vesta.wrapper.security;

import java.util.concurrent.ConcurrentHashMap;

/**
 * Per-key token bucket. Capacity equals the configured permits-per-window;
 * tokens refill continuously over the window duration.
 */
public final class TokenBucketRateLimiter {

    public static final class Decision {
        public final boolean allowed;
        /** Seconds until at least one token is available (0 when allowed). */
        public final long retryAfterSeconds;

        public Decision(boolean allowed, long retryAfterSeconds) {
            this.allowed = allowed;
            this.retryAfterSeconds = Math.max(0L, retryAfterSeconds);
        }
    }

    private static final class Bucket {
        double tokens;
        long updatedAtNanos;

        Bucket(double capacity, long nowNanos) {
            this.tokens = capacity;
            this.updatedAtNanos = nowNanos;
        }
    }

    private final double capacity;
    private final double refillPerNanos;
    private final ConcurrentHashMap<String, Bucket> buckets = new ConcurrentHashMap<>();

    /**
     * @param permitsPerWindow maximum burst / steady-state rate
     * @param windowMillis     refill window (e.g. 60_000 for per-minute)
     */
    public TokenBucketRateLimiter(int permitsPerWindow, long windowMillis) {
        if (permitsPerWindow < 1) {
            throw new IllegalArgumentException("permitsPerWindow must be >= 1");
        }
        if (windowMillis < 1L) {
            throw new IllegalArgumentException("windowMillis must be >= 1");
        }
        this.capacity = permitsPerWindow;
        this.refillPerNanos = capacity / (windowMillis * 1_000_000.0);
    }

    public Decision tryAcquire(String key) {
        return tryAcquire(key, System.nanoTime());
    }

    /** Package-visible for deterministic unit tests. */
    Decision tryAcquire(String key, long nowNanos) {
        String bucketKey = key == null || key.isBlank() ? "unknown" : key;
        Bucket bucket = buckets.computeIfAbsent(bucketKey, ignored -> new Bucket(capacity, nowNanos));
        synchronized (bucket) {
            refill(bucket, nowNanos);
            if (bucket.tokens >= 1.0) {
                bucket.tokens -= 1.0;
                return new Decision(true, 0L);
            }
            double deficit = 1.0 - bucket.tokens;
            long retryNanos = (long) Math.ceil(deficit / refillPerNanos);
            long retryAfterSeconds = Math.max(1L, (retryNanos + 999_999_999L) / 1_000_000_000L);
            return new Decision(false, retryAfterSeconds);
        }
    }

    private void refill(Bucket bucket, long nowNanos) {
        long elapsed = Math.max(0L, nowNanos - bucket.updatedAtNanos);
        if (elapsed > 0L) {
            bucket.tokens = Math.min(capacity, bucket.tokens + elapsed * refillPerNanos);
            bucket.updatedAtNanos = nowNanos;
        }
    }
}
