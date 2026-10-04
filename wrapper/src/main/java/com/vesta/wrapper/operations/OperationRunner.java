package com.vesta.wrapper.operations;

import com.vesta.wrapper.state.StateDirectory;
import com.vesta.wrapper.state.StateException;
import org.slf4j.Logger;
import org.slf4j.LoggerFactory;

import java.security.SecureRandom;
import java.time.Duration;
import java.util.ArrayList;
import java.util.Base64;
import java.util.List;
import java.util.Objects;
import java.util.Optional;
import java.util.Set;

/**
 * Lifecycle-owned executor for durable operations using registered typed handlers.
 *
 * <p>Invariants:
 * <ul>
 *   <li>One runner per state directory across processes ({@link StateDirectory#acquireRunnerLock()}).
 *       Startup recovery runs only after the lock is held.</li>
 *   <li>Each claim carries a random owner token; lifecycle transitions require it. The token is never
 *       logged, audited, emitted or returned outside this package's records.</li>
 *   <li>Fail closed: unknown handler, contract/resource mismatch, unavailable input, parse error and
 *       authority denial fail the operation before any effect. After a non-idempotent effect began,
 *       any unknown outcome becomes recovery_required, never a replay.</li>
 *   <li>If a state/audit write fails, the runner stops claiming ({@link #healthy()} false) and leaves
 *       the row for restart recovery.</li>
 *   <li>Shutdown never releases the runner lock, requeues work or clears locks while a worker thread is
 *       alive. Work is only requeued by the worker itself at a declared safe point. If workers outlive
 *       the grace period and interruption, the lock stays held until they actually exit (or the
 *       process ends). Java code cannot be forcibly stopped.</li>
 * </ul>
 * No handlers are built in, and this class exposes nothing over the network.
 */
public final class OperationRunner implements AutoCloseable {
    private static final Logger LOG = LoggerFactory.getLogger(OperationRunner.class);
    private static final SecureRandom RANDOM = new SecureRandom();
    static final long PROGRESS_MIN_INTERVAL_MS = 250;

    /** Runner tuning. */
    public record Settings(int workers, Duration pollInterval, Duration shutdownGrace) {
        public Settings {
            if (workers < 1 || workers > 8) {
                throw new IllegalArgumentException("workers must be 1..8");
            }
            Objects.requireNonNull(pollInterval, "pollInterval");
            Objects.requireNonNull(shutdownGrace, "shutdownGrace");
            if (pollInterval.toMillis() < 10 || pollInterval.compareTo(Duration.ofMinutes(1)) > 0) {
                throw new IllegalArgumentException("pollInterval must be 10ms..1m");
            }
            if (shutdownGrace.isNegative() || shutdownGrace.compareTo(Duration.ofMinutes(10)) > 0) {
                throw new IllegalArgumentException("shutdownGrace must be 0..10m");
            }
        }

        public static Settings defaults() {
            return new Settings(1, Duration.ofSeconds(1), Duration.ofSeconds(30));
        }
    }

    private final StateDirectory directory;
    private final OperationService ops;
    private final HandlerRegistry handlers;
    private final OperationAuthority authority;
    private final Settings settings;
    private final Object monitor = new Object();
    private final List<Thread> workers = new ArrayList<>();

    private volatile boolean stopping;
    private volatile boolean healthy = true;
    private volatile boolean lockReleased;
    private boolean started;
    private AutoCloseable runnerLock;

    public OperationRunner(StateDirectory directory, OperationService ops, HandlerRegistry handlers,
                           OperationAuthority authority, Settings settings) {
        this.directory = Objects.requireNonNull(directory, "directory");
        this.ops = Objects.requireNonNull(ops, "ops");
        this.handlers = Objects.requireNonNull(handlers, "handlers");
        this.authority = Objects.requireNonNull(authority, "authority");
        this.settings = Objects.requireNonNull(settings, "settings");
        // The runner lock must guard exactly the database this runner reconciles and mutates.
        // Both paths are canonicalized (real path of the deepest existing ancestor) by StateDirectory.
        if (!directory.path().equals(ops.database().directory().path())) {
            throw new StateException("runner_directory_mismatch",
                    "Runner state directory differs from the operation database's state directory");
        }
    }

    /** Acquires exclusive runner ownership, reconciles interrupted work, then starts workers. */
    public synchronized void start() {
        if (started) {
            throw new IllegalStateException("Runner already started");
        }
        started = true;
        runnerLock = directory.acquireRunnerLock();
        try {
            List<Operation> recovered = ops.recoverAfterRestart();
            if (!recovered.isEmpty()) {
                LOG.info("Reconciled {} interrupted or unexecutable operation(s) at startup", recovered.size());
            }
        } catch (RuntimeException e) {
            healthy = false;
            releaseLock();
            throw e;
        }
        try {
            for (int i = 0; i < settings.workers(); i++) {
                Thread t = new Thread(this::workerLoop, "vesta-op-runner-" + i);
                t.setDaemon(true); // process exit releases the OS lock and ends any stuck handler
                // Track before start: an unstarted tracked thread is never alive (harmless), whereas an
                // untracked started thread could outlive a lock release.
                workers.add(t);
                t.start();
            }
        } catch (Throwable t) {
            // Partial startup: stop any started workers; keep the lock until they have all exited.
            healthy = false;
            LOG.error("Operation runner failed to start all workers: {}", t.getClass().getSimpleName());
            shutdownWorkers();
            if (t instanceof Error e) {
                throw e;
            }
            throw new StateException("runner_start_failed", "Operation runner failed to start workers", t);
        }
    }

    /** Hint that new work may be queued. */
    public void wake() {
        synchronized (monitor) {
            monitor.notifyAll();
        }
    }

    /** False once a durable state/audit write failed; the runner then stops claiming new work. */
    public boolean healthy() {
        return healthy;
    }

    /** True when every worker has exited and runner ownership was released. */
    public boolean fullyStopped() {
        return lockReleased;
    }

    /**
     * Stops claiming, asks running handlers to stop at their next safe point, waits the grace period,
     * interrupts, waits again. Ownership is released only after every worker has exited; otherwise a
     * reaper keeps the lock until they do.
     */
    @Override
    public synchronized void close() {
        if (!started || stopping) {
            return;
        }
        shutdownWorkers();
    }

    /** Stops workers; releases ownership only once every started worker has exited. */
    private void shutdownWorkers() {
        stopping = true;
        wake();
        long graceMs = settings.shutdownGrace().toMillis();
        joinAll(graceMs);
        if (anyAlive()) {
            for (Thread t : workers) {
                t.interrupt();
            }
            joinAll(graceMs);
        }
        if (!anyAlive()) {
            releaseLock();
            return;
        }
        LOG.warn("Operation handlers did not stop within the grace period; runner ownership retained until they exit");
        Thread reaper = new Thread(() -> {
            for (Thread t : workers) {
                while (t.isAlive()) {
                    try {
                        t.join();
                    } catch (InterruptedException e) {
                        // keep waiting: ownership must outlive the worker
                    }
                }
            }
            releaseLock();
        }, "vesta-op-runner-reaper");
        reaper.setDaemon(true);
        reaper.start();
    }

    // ------------------------------------------------------------------ worker

    private void workerLoop() {
        while (!stopping && healthy) {
            Optional<ClaimedOperation> claim;
            try {
                claim = ops.claimNext(newToken());
            } catch (RuntimeException e) {
                degrade("claim", e);
                return;
            }
            if (claim.isPresent()) {
                execute(claim.get());
                continue;
            }
            synchronized (monitor) {
                if (stopping) {
                    return;
                }
                try {
                    monitor.wait(settings.pollInterval().toMillis());
                } catch (InterruptedException e) {
                    if (stopping) {
                        return;
                    }
                }
            }
        }
    }

    private void execute(ClaimedOperation claim) {
        Execution ex = new Execution(claim);
        try {
            try {
                run(ex);
            } catch (StopSignal s) {
                if (s.kind == StopSignal.Kind.CANCEL) {
                    ops.acknowledgeCancelled(ex.id, ex.token);
                } else {
                    ops.suspendForShutdown(ex.id, ex.token);
                }
            } catch (AuthorityDenied d) {
                terminalForError(ex, "authority_denied", "Current authority denied: " + d.reason);
            } catch (OperationFailure f) {
                terminalForKnownFailure(ex, f);
            } catch (StateException e) {
                throw e; // durable-state failure: handled below, never masked as a handler outcome
            } catch (Throwable t) {
                if (stopping && (t instanceof InterruptedException || Thread.currentThread().isInterrupted())) {
                    Thread.interrupted();
                    ops.suspendForShutdown(ex.id, ex.token);
                } else {
                    LOG.warn("Operation handler error for {} ({}): {}", ex.id, ex.snapshot.type(), t.getClass().getName());
                    terminalForError(ex, "handler_error", "Handler failed: " + t.getClass().getSimpleName());
                }
            }
        } catch (StateException e) {
            degrade("operation " + ex.id, e);
        }
    }

    private void run(Execution ex) throws Exception {
        Operation op = ex.snapshot;
        OperationHandler<?> handler = handlers.find(op.type()).orElse(null);
        if (handler == null) {
            ops.fail(ex.id, ex.token, "unknown_handler", "No registered handler for this operation type");
            return;
        }
        HandlerContract contract = handler.contract();
        ex.contract = contract;
        if (contract.externalEffectIdempotent() != op.externalEffectIdempotent()
                || contract.requiresConfirmation() != op.requiresConfirmation()) {
            ops.fail(ex.id, ex.token, "handler_contract_mismatch", "Operation flags differ from the handler contract");
            return;
        }
        ex.authorize(OperationAuthority.Stage.CLAIM);
        OperationInput input = ops.input(ex.id).orElse(null);
        if (input == null || input.status() != OperationInput.InputStatus.AVAILABLE) {
            ops.fail(ex.id, ex.token, OperationService.INPUT_UNAVAILABLE, "Stored operation input is unavailable");
            return;
        }
        runTyped(handler, input.canonicalJson(), ex);
    }

    private <I> void runTyped(OperationHandler<I> handler, String canonicalJson, Execution ex) throws Exception {
        I input;
        try {
            input = handler.parseInput(canonicalJson);
        } catch (OperationFailure f) {
            ops.fail(ex.id, ex.token, "invalid_input", f.getMessage());
            return;
        } catch (RuntimeException e) {
            ops.fail(ex.id, ex.token, "invalid_input", "Operation input could not be parsed");
            return;
        }
        Set<String> required = handler.requiredResources(input);
        if (required == null || !ex.snapshot.lockResources().containsAll(required)) {
            ops.fail(ex.id, ex.token, "resource_contract_mismatch", "Operation does not hold the handler's required locks");
            return;
        }
        ex.checkpoint();
        ex.authorize(OperationAuthority.Stage.BEFORE_RUN); // after potentially blocking parse/preparation
        try {
            ex.snapshot = ops.markRunning(ex.id, ex.token);
        } catch (OperationException e) {
            if (!OperationException.INVALID_TRANSITION.equals(e.getCode())) {
                throw e;
            }
            ex.checkpoint(); // cancel raced the transition: becomes a cancel signal
            throw e;
        }
        String result = handler.execute(input, ex);
        if (result != null && result.length() > OperationService.MAX_RESULT_CHARS) {
            terminalForError(ex, "result_too_large", "Handler result exceeded the size limit");
            return;
        }
        ops.succeed(ex.id, ex.token, result);
    }

    /** Known failure declared by the handler. */
    private void terminalForKnownFailure(Execution ex, OperationFailure f) {
        ops.fail(ex.id, ex.token, f.getCode(), f.getMessage());
    }

    /** Unknown outcome: recovery_required if a non-idempotent effect may have happened, else failed. */
    private void terminalForError(Execution ex, String code, String message) {
        Operation current = ops.get(ex.id).orElseThrow(() -> OperationService.notFound(ex.id));
        if (current.externalEffectStarted() && !current.externalEffectIdempotent()) {
            ops.markRecoveryRequired(ex.id, ex.token, code);
        } else {
            ops.fail(ex.id, ex.token, code, message);
        }
    }

    private void degrade(String where, RuntimeException e) {
        healthy = false;
        LOG.error("Operation runner stopped claiming after a durable state failure in {}: {}", where,
                e instanceof StateException se ? se.getCode() : e.getClass().getSimpleName());
        wake();
    }

    private boolean anyAlive() {
        for (Thread t : workers) {
            if (t.isAlive()) {
                return true;
            }
        }
        return false;
    }

    private void joinAll(long budgetMs) {
        long deadline = System.nanoTime() + budgetMs * 1_000_000L;
        for (Thread t : workers) {
            long remaining = (deadline - System.nanoTime()) / 1_000_000L;
            if (remaining <= 0) {
                return;
            }
            try {
                t.join(remaining);
            } catch (InterruptedException e) {
                Thread.currentThread().interrupt();
                return;
            }
        }
    }

    private synchronized void releaseLock() {
        if (lockReleased || runnerLock == null) {
            return;
        }
        try {
            runnerLock.close();
        } catch (Exception e) {
            LOG.warn("Failed to release operation runner lock: {}", e.getClass().getSimpleName());
        }
        lockReleased = true;
    }

    private static String newToken() {
        byte[] raw = new byte[24];
        RANDOM.nextBytes(raw);
        return Base64.getUrlEncoder().withoutPadding().encodeToString(raw);
    }

    // ------------------------------------------------------------------ execution context

    // Signals extend Error so handler code catching Exception cannot accidentally swallow them.
    private static final class StopSignal extends Error {
        enum Kind { CANCEL, SHUTDOWN }

        final Kind kind;

        StopSignal(Kind kind) {
            super(null, null, false, false);
            this.kind = kind;
        }
    }

    private static final class AuthorityDenied extends Error {
        final String reason;

        AuthorityDenied(String reason) {
            super(null, null, false, false);
            this.reason = reason;
        }
    }

    private final class Execution implements OperationContext {
        final String id;
        final String token;
        volatile Operation snapshot;
        HandlerContract contract;
        long lastProgressMs = Long.MIN_VALUE / 2;

        Execution(ClaimedOperation claim) {
            this.id = claim.operation().id();
            this.token = claim.ownerToken();
            this.snapshot = claim.operation();
        }

        @Override
        public Operation operation() {
            return snapshot;
        }

        private Operation refresh() {
            snapshot = ops.get(id).orElseThrow(() -> OperationService.notFound(id));
            return snapshot;
        }

        @Override
        public void checkpoint() {
            if (stopping) {
                throw new StopSignal(StopSignal.Kind.SHUTDOWN);
            }
            Operation op = refresh();
            if (op.state() == OperationState.CANCELLING
                    && (!op.externalEffectStarted() || (contract != null && contract.cancellableAfterEffect()))) {
                throw new StopSignal(StopSignal.Kind.CANCEL);
            }
        }

        @Override
        public boolean cancellationRequested() {
            return refresh().cancelRequested();
        }

        @Override
        public void progress(int percent, String message) {
            long now = ops.database().clock().millis();
            if (percent < 100 && now - lastProgressMs < PROGRESS_MIN_INTERVAL_MS) {
                return;
            }
            lastProgressMs = now;
            snapshot = ops.reportProgress(id, token, percent, message);
        }

        @Override
        public void beginExternalEffect() {
            checkpoint();
            authorize(OperationAuthority.Stage.BEFORE_EXTERNAL_EFFECT);
            if (snapshot.externalEffectStarted()) {
                return; // already durably marked; authority rechecked for this effect
            }
            try {
                snapshot = ops.markExternalEffectStarted(id, token);
            } catch (OperationException e) {
                if (OperationException.INVALID_TRANSITION.equals(e.getCode())) {
                    checkpoint(); // cancellation raced: stop before the effect
                }
                throw e;
            }
        }

        void authorize(OperationAuthority.Stage stage) {
            Operation op = refresh();
            OperationAuthority.Decision decision;
            try {
                decision = authority.recheck(op, stage);
            } catch (RuntimeException e) {
                decision = OperationAuthority.Decision.deny("authority_check_failed");
            }
            if (decision == null || !decision.allowed()) {
                throw new AuthorityDenied(decision == null ? "authority_check_failed" : decision.reason());
            }
        }
    }
}
