package com.vesta.wrapper.operations;

/** Runner-provided view of the executing operation. Not thread-safe; use from the handler thread. */
public interface OperationContext {

    /** Latest known snapshot of the operation (refreshed at checkpoints). */
    Operation operation();

    /**
     * Safe point. Throws an internal stop signal (do not catch it) when the runner is shutting
     * down, or when cancellation was requested and the handler contract permits stopping here.
     */
    void checkpoint();

    /** Whether cancellation has been requested (read from durable state). */
    boolean cancellationRequested();

    /** Persists progress (rate-limited; message bounded to 256 chars). */
    void progress(int percent, String message);

    /**
     * Must be called immediately before each external effect. Honors pending cancel/shutdown first,
     * rechecks current authority, then durably records that an effect may have started.
     */
    void beginExternalEffect();
}
