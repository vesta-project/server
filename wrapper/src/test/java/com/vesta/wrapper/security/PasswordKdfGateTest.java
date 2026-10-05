package com.vesta.wrapper.security;

import org.junit.jupiter.api.Test;

import java.time.Duration;
import java.util.ArrayList;
import java.util.List;
import java.util.concurrent.CountDownLatch;
import java.util.concurrent.Executors;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.atomic.AtomicLong;

import static com.vesta.wrapper.security.PasswordKdfGate.Purpose.*;
import static com.vesta.wrapper.security.PasswordKdfGate.Status.*;
import static org.junit.jupiter.api.Assertions.*;

class PasswordKdfGateTest {
    private final AtomicLong nanos = new AtomicLong();

    private PasswordKdfGate gate(int concurrent, int verification, int management) {
        return new PasswordKdfGate(new PasswordKdfGate.Settings(concurrent, verification, management), nanos::get);
    }

    @Test void productionGateIsSharedAndDefaultsBoundAllThreeBudgets() {
        assertSame(PasswordKdfGate.shared(), PasswordKdfGate.shared());
        assertEquals(new PasswordKdfGate.Settings(2, 60, 10), PasswordKdfGate.Settings.defaults());
    }

    @Test void exactRateBoundaryAndContinuousRefillAreInclusive() {
        var gate = gate(1, 2, 1);
        for (int i = 0; i < 2; i++) {
            var admitted = gate.tryAcquire(VERIFICATION);
            assertEquals(ADMITTED, admitted.status()); admitted.permit().close();
        }
        assertEquals(RATE_LIMITED, gate.tryAcquire(VERIFICATION).status());
        nanos.set(Duration.ofSeconds(30).toNanos() - 1);
        assertEquals(RATE_LIMITED, gate.tryAcquire(VERIFICATION).status());
        nanos.incrementAndGet();
        var refilled = gate.tryAcquire(VERIFICATION);
        assertEquals(ADMITTED, refilled.status()); refilled.permit().close();
        assertEquals(RATE_LIMITED, gate.tryAcquire(VERIFICATION).status());
    }

    @Test void verificationAndManagementRatesAreIndependentButShareConcurrency() {
        var gate = gate(1, 1, 2);
        var first = gate.tryAcquire(VERIFICATION);
        assertEquals(ADMITTED, first.status());
        assertEquals(BUSY, gate.tryAcquire(MANAGEMENT).status());
        assertEquals(RATE_LIMITED, gate.tryAcquire(VERIFICATION).status());
        first.permit().close();
        var mutation = gate.tryAcquire(MANAGEMENT);
        assertEquals(ADMITTED, mutation.status()); mutation.permit().close();
        assertEquals(RATE_LIMITED, gate.tryAcquire(MANAGEMENT).status());
    }

    @Test void closingAPermitTwiceDoesNotCreateAnotherSlot() {
        var gate = gate(1, 10, 10);
        var first = gate.tryAcquire(VERIFICATION);
        first.permit().close(); first.permit().close();
        var replacement = gate.tryAcquire(VERIFICATION);
        assertEquals(ADMITTED, replacement.status());
        assertEquals(BUSY, gate.tryAcquire(MANAGEMENT).status());
        replacement.permit().close();
    }

    @Test void clockRollbackCannotRefillOrResetTheBucket() {
        var gate = gate(1, 1, 1);
        var first = gate.tryAcquire(VERIFICATION); first.permit().close();
        nanos.set(-Duration.ofDays(1).toNanos());
        assertEquals(RATE_LIMITED, gate.tryAcquire(VERIFICATION).status());
        nanos.set(Duration.ofSeconds(60).toNanos() - 1);
        assertEquals(RATE_LIMITED, gate.tryAcquire(VERIFICATION).status());
        nanos.incrementAndGet();
        var next = gate.tryAcquire(VERIFICATION); assertEquals(ADMITTED, next.status()); next.permit().close();
    }

    @Test void longIdleTimeRefillsOnlyToCapacity() {
        var gate = gate(1, 1, 1);
        var first = gate.tryAcquire(VERIFICATION); first.permit().close();
        nanos.set(Duration.ofDays(10).toNanos());
        var next = gate.tryAcquire(VERIFICATION); next.permit().close();
        assertEquals(RATE_LIMITED, gate.tryAcquire(VERIFICATION).status());
    }

    @Test void cheapRevocationAdmissionDoesNotWaitForDerivationSlots() {
        var gate = gate(1, 2, 1);
        var first = gate.tryAcquire(VERIFICATION);
        assertTrue(gate.tryConsumeAttempt(MANAGEMENT));
        assertFalse(gate.tryConsumeAttempt(MANAGEMENT));
        assertEquals(BUSY, gate.tryAcquire(VERIFICATION).status());
        first.permit().close();
    }

    @Test void concurrentAdmissionNeverExceedsTheCeilingAndEverySlotCanBeReused() throws Exception {
        var gate = gate(2, 100, 100);
        CountDownLatch start = new CountDownLatch(1);
        var executor = Executors.newFixedThreadPool(16);
        List<java.util.concurrent.Future<PasswordKdfGate.Admission>> futures = new ArrayList<>();
        List<PasswordKdfGate.Permit> owned = new ArrayList<>();
        try {
            for (int i = 0; i < 16; i++) futures.add(executor.submit(() -> {
                assertTrue(start.await(5, TimeUnit.SECONDS)); return gate.tryAcquire(VERIFICATION);
            }));
            start.countDown();
            int busy = 0;
            for (var future : futures) {
                var admission = future.get(5, TimeUnit.SECONDS);
                if (admission.status() == ADMITTED) owned.add(admission.permit());
                else { assertEquals(BUSY, admission.status()); busy++; }
            }
            assertEquals(2, owned.size()); assertEquals(14, busy);
            owned.forEach(PasswordKdfGate.Permit::close); owned.clear();
            for (int i = 0; i < 2; i++) {
                var admission = gate.tryAcquire(MANAGEMENT); assertEquals(ADMITTED, admission.status()); owned.add(admission.permit());
            }
            assertEquals(BUSY, gate.tryAcquire(MANAGEMENT).status());
        } finally {
            owned.forEach(PasswordKdfGate.Permit::close); start.countDown(); executor.shutdownNow();
            assertTrue(executor.awaitTermination(5, TimeUnit.SECONDS));
        }
    }

    @Test void malformedSettingsAndAdmissionPairsAreRejected() {
        for (int[] invalid : new int[][]{{0,1,1},{9,1,1},{1,0,1},{1,10001,1},{1,1,0},{1,1,1001}}) {
            assertThrows(IllegalArgumentException.class, () -> new PasswordKdfGate.Settings(invalid[0], invalid[1], invalid[2]));
        }
        assertThrows(IllegalArgumentException.class, () -> new PasswordKdfGate.Admission(ADMITTED, null));
        assertThrows(NullPointerException.class, () -> gate(1,1,1).tryAcquire(null));
    }
}
