package com.vesta.wrapper.security;

import org.junit.jupiter.api.Test;

import static org.junit.jupiter.api.Assertions.*;

class TokenBucketRateLimiterTest {

    @Test
    void allowsBurstUpToCapacityThenRejects() {
        TokenBucketRateLimiter limiter = new TokenBucketRateLimiter(3, 60_000L);
        long t0 = 1_000_000_000L;
        assertTrue(limiter.tryAcquire("127.0.0.1", t0).allowed);
        assertTrue(limiter.tryAcquire("127.0.0.1", t0 + 1).allowed);
        assertTrue(limiter.tryAcquire("127.0.0.1", t0 + 2).allowed);
        TokenBucketRateLimiter.Decision denied = limiter.tryAcquire("127.0.0.1", t0 + 3);
        assertFalse(denied.allowed);
        assertTrue(denied.retryAfterSeconds >= 1L);
    }

    @Test
    void refillsOverWindow() {
        TokenBucketRateLimiter limiter = new TokenBucketRateLimiter(2, 1_000L);
        long t0 = 5_000_000_000L;
        assertTrue(limiter.tryAcquire("a", t0).allowed);
        assertTrue(limiter.tryAcquire("a", t0).allowed);
        assertFalse(limiter.tryAcquire("a", t0).allowed);

        // After a full second, capacity is restored.
        assertTrue(limiter.tryAcquire("a", t0 + 1_000_000_000L).allowed);
    }

    @Test
    void isolatesKeys() {
        TokenBucketRateLimiter limiter = new TokenBucketRateLimiter(1, 60_000L);
        long t0 = 10_000_000_000L;
        assertTrue(limiter.tryAcquire("ip-a", t0).allowed);
        assertFalse(limiter.tryAcquire("ip-a", t0 + 1).allowed);
        assertTrue(limiter.tryAcquire("ip-b", t0 + 2).allowed);
    }

    @Test
    void retryAfterReflectsRefillWindow() {
        TokenBucketRateLimiter limiter = new TokenBucketRateLimiter(1, 60_000L);
        long t0 = 20_000_000_000L;
        assertTrue(limiter.tryAcquire("x", t0).allowed);
        TokenBucketRateLimiter.Decision denied = limiter.tryAcquire("x", t0 + 1);
        assertFalse(denied.allowed);
        // One token per minute → ~60s until the next permit.
        assertEquals(60L, denied.retryAfterSeconds);
    }
}
