package com.vesta.wrapper.security;

import java.util.Objects;
import java.util.concurrent.Semaphore;
import java.util.concurrent.atomic.AtomicBoolean;
import java.util.function.LongSupplier;

/**
 * Nonblocking admission for pack-password work. Production services share two fixed-key rate buckets
 * and a concurrency ceiling; there is no waiting queue or client-controlled bookkeeping key.
 * Budgets are JVM-local and reset on restart, not persistent/per-IP abuse controls or a fleet quota.
 */
public final class PasswordKdfGate {
    public enum Purpose { MANAGEMENT, VERIFICATION }
    public enum Status { ADMITTED, RATE_LIMITED, BUSY }

    public record Settings(int concurrentDerivations, int verificationPerMinute, int managementPerMinute) {
        public Settings {
            if (concurrentDerivations < 1 || concurrentDerivations > 8
                    || verificationPerMinute < 1 || verificationPerMinute > 10_000
                    || managementPerMinute < 1 || managementPerMinute > 1_000) {
                throw new IllegalArgumentException("Invalid password admission settings");
            }
        }

        public static Settings defaults() {
            return new Settings(2, 60, 10);
        }
    }

    /** Releases only this admission, exactly once, even when cleanup is repeated. */
    public static final class Permit implements AutoCloseable {
        private final Semaphore semaphore;
        private final AtomicBoolean closed = new AtomicBoolean();

        private Permit(Semaphore semaphore) {
            this.semaphore = semaphore;
        }

        @Override
        public void close() {
            if (closed.compareAndSet(false, true)) {
                semaphore.release();
            }
        }

        @Override
        public String toString() {
            return "PasswordKdfPermit[closed=" + closed.get() + "]";
        }
    }

    public record Admission(Status status, Permit permit) {
        public Admission {
            Objects.requireNonNull(status, "status");
            if ((status == Status.ADMITTED) != (permit != null)) {
                throw new IllegalArgumentException("Invalid password admission");
            }
        }
    }

    private static final PasswordKdfGate SHARED = new PasswordKdfGate(Settings.defaults());
    private final Semaphore semaphore;
    private final TokenBucketRateLimiter verification;
    private final TokenBucketRateLimiter management;
    private final LongSupplier monotonicNanos;

    public PasswordKdfGate(Settings settings) {
        this(settings, System::nanoTime);
    }

    /** Trusted deterministic clock seam; never derived from a request. */
    PasswordKdfGate(Settings settings, LongSupplier monotonicNanos) {
        Objects.requireNonNull(settings, "settings");
        this.monotonicNanos = Objects.requireNonNull(monotonicNanos, "monotonicNanos");
        semaphore = new Semaphore(settings.concurrentDerivations());
        verification = new TokenBucketRateLimiter(settings.verificationPerMinute(), 60_000);
        management = new TokenBucketRateLimiter(settings.managementPerMinute(), 60_000);
    }

    public static PasswordKdfGate shared() {
        return SHARED;
    }

    /** Admission for cheap administrative revocation, which needs no KDF/concurrency permit. */
    public boolean tryConsumeAttempt(Purpose purpose) {
        Objects.requireNonNull(purpose, "purpose");
        TokenBucketRateLimiter bucket = purpose == Purpose.MANAGEMENT ? management : verification;
        return bucket.tryAcquire("pack-password", monotonicNanos.getAsLong()).allowed;
    }

    /** All attempts consume rate budget, including attempts rejected for concurrent saturation. */
    public Admission tryAcquire(Purpose purpose) {
        Objects.requireNonNull(purpose, "purpose");
        if (!tryConsumeAttempt(purpose)) {
            return new Admission(Status.RATE_LIMITED, null);
        }
        if (!semaphore.tryAcquire()) {
            return new Admission(Status.BUSY, null);
        }
        return new Admission(Status.ADMITTED, new Permit(semaphore));
    }
}
