package com.vesta.wrapper.runtime;

import com.vesta.wrapper.audit.AuditLog;
import com.vesta.wrapper.authorization.AuthorizationService;
import com.vesta.wrapper.config.WrapperConfig;
import com.vesta.wrapper.identity.IdentityService;
import com.vesta.wrapper.identity.LocalAuthority;
import com.vesta.wrapper.operations.ConfirmationService;
import com.vesta.wrapper.operations.OperationBindings;
import com.vesta.wrapper.operations.OperationRunner;
import com.vesta.wrapper.operations.OperationService;
import com.vesta.wrapper.operations.OperationSubmissionService;
import com.vesta.wrapper.operations.PolicyConfirmationService;
import com.vesta.wrapper.operations.PolicyOperationAuthority;
import com.vesta.wrapper.operations.lifecycle.LifecycleBindings;
import com.vesta.wrapper.state.EventLog;
import com.vesta.wrapper.state.StateDatabase;
import com.vesta.wrapper.state.StateDirectory;
import com.vesta.wrapper.state.StateException;
import org.slf4j.Logger;
import org.slf4j.LoggerFactory;

import java.io.IOException;
import java.nio.file.Files;
import java.nio.file.LinkOption;
import java.nio.file.Path;
import java.nio.file.attribute.BasicFileAttributes;
import java.time.Clock;
import java.util.Objects;

/**
 * Composition root of the durable agent runtime: protected state directory and database, audit and event
 * logs, identity/authorization (with the given trusted in-process {@link LocalAuthority}; production uses
 * {@link LocalAuthority#UNAVAILABLE}), typed lifecycle bindings, the canonical policy authority, the
 * submission boundary, the confirmation gateway and the operation runner (started: runner lock acquired,
 * interrupted work reconciled, workers running).
 *
 * <p>Scope and limits (deliberately honest):
 * <ul>
 *   <li>No network route is exposed; nothing here accepts remote identity yet.</li>
 *   <li>The Minecraft process remains a child of this JVM (wrapper-owned). There is no independent
 *       supervisor: if the wrapper exits, the game's lifetime is not durably managed.</li>
 *   <li>{@link #open} never starts the game. Legacy auto-start (outside operation scope, unaudited) is the
 *       caller's decision and must happen only after the runtime opened.</li>
 *   <li>The state database's canonical server id is persisted independently of the legacy advertised
 *       {@link WrapperConfig#serverId}; no mapping between them is made here.</li>
 * </ul>
 */
public final class AgentRuntime implements AutoCloseable {
    private static final Logger LOG = LoggerFactory.getLogger(AgentRuntime.class);

    private final StateDirectory stateDirectory;
    private final StateDatabase database;
    private final AuditLog audit;
    private final EventLog events;
    private final OperationService operations;
    private final IdentityService identities;
    private final AuthorizationService authorization;
    private final OperationBindings bindings;
    private final OperationSubmissionService submissions;
    private final ConfirmationService confirmationTokens;
    private final PolicyConfirmationService confirmations;
    private final LifecycleBindings.GenerationRevision generationRevision;
    private final OperationRunner runner;
    private final MinecraftServerProcess game;

    private boolean shutdownRequested;
    private volatile boolean databaseClosed;

    private AgentRuntime(StateDirectory stateDirectory, StateDatabase database, AuditLog audit, EventLog events,
                         OperationService operations, IdentityService identities, OperationBindings bindings,
                         OperationSubmissionService submissions, ConfirmationService confirmationTokens,
                         PolicyConfirmationService confirmations,
                         LifecycleBindings.GenerationRevision generationRevision, OperationRunner runner,
                         MinecraftServerProcess game) {
        this.stateDirectory = stateDirectory;
        this.database = database;
        this.audit = audit;
        this.events = events;
        this.operations = operations;
        this.identities = identities;
        this.authorization = identities.authorization();
        this.bindings = bindings;
        this.submissions = submissions;
        this.confirmationTokens = confirmationTokens;
        this.confirmations = confirmations;
        this.generationRevision = generationRevision;
        this.runner = runner;
        this.game = game;
    }

    /** Production composition: system UTC clock, no local authority, default runner settings. */
    public static AgentRuntime open(WrapperConfig config, MinecraftServerProcess game) {
        return open(config, game, Clock.systemUTC(), LocalAuthority.UNAVAILABLE, OperationRunner.Settings.defaults());
    }

    /**
     * Trusted in-process composition. {@code localAuthority} must be a trusted in-process implementation;
     * it is never derived from network input.
     *
     * @throws StateException e.g. {@code state_dir_overlaps_game_root}, {@code runner_lock_held}; on failure
     *                        everything opened so far is closed (the database is retained only if runner
     *                        workers could still be alive)
     */
    public static AgentRuntime open(WrapperConfig config, MinecraftServerProcess game, Clock clock,
                                    LocalAuthority localAuthority, OperationRunner.Settings runnerSettings) {
        Objects.requireNonNull(config, "config");
        Objects.requireNonNull(game, "game");
        Objects.requireNonNull(clock, "clock");
        Objects.requireNonNull(localAuthority, "localAuthority");
        Objects.requireNonNull(runnerSettings, "runnerSettings");
        Objects.requireNonNull(config.stateDirectory, "config.stateDirectory");

        // Reject an already-present link/junction/reparse point in the selected root or any ancestor BEFORE
        // StateDirectory canonicalizes it away and before anything is created or written.
        rejectLinkedPath(config.stateDirectory);
        // Explicit overlap validation against every game-visible root, in both directions.
        StateDirectory directory = new StateDirectory(config.stateDirectory, config.minecraftWorkingDirectory);
        new StateDirectory(config.stateDirectory, config.scanRoot);

        StateDatabase db = StateDatabase.open(directory, clock);
        OperationRunner runner = null;
        try {
            // Best-effort re-check after creation (not a TOCTOU defence against a same-account attacker).
            rejectLinkedPath(config.stateDirectory);
            AuditLog audit = new AuditLog(db);
            EventLog events = new EventLog(db);
            OperationService ops = new OperationService(db, audit, events);
            IdentityService identities = new IdentityService(db, audit, localAuthority);
            AuthorizationService authorization = identities.authorization();
            OperationBindings bindings = OperationBindings.of(LifecycleBindings.create(game));
            PolicyOperationAuthority authority = new PolicyOperationAuthority(ops, authorization, bindings);
            OperationSubmissionService submissions = new OperationSubmissionService(ops, authorization, bindings);
            ConfirmationService tokens = new ConfirmationService(db, audit, ops);
            LifecycleBindings.GenerationRevision revision = new LifecycleBindings.GenerationRevision(game);
            PolicyConfirmationService confirmations =
                    new PolicyConfirmationService(ops, tokens, authorization, bindings, revision);
            runner = new OperationRunner(directory, ops, bindings.handlers(), authority, runnerSettings);
            runner.start();
            LOG.info("Durable agent runtime opened (state outside game root; no network operation route)");
            return new AgentRuntime(directory, db, audit, events, ops, identities, bindings, submissions, tokens,
                    confirmations, revision, runner, game);
        } catch (Throwable t) {
            // 1. Stop our runner first (bounded). Safe in every start stage: if lock acquisition failed it
            //    has no workers and no lock, and close() releases nothing it does not hold (another
            //    runtime's lock/database are never touched; `db` here is only this call's own instance).
            if (runner != null) {
                try {
                    runner.close();
                } catch (RuntimeException closeFailure) {
                    t.addSuppressed(closeFailure);
                }
            }
            // 2. Decide by THIS runner's ownership: OperationRunner.fullyStopped() is true only when none of
            //    its workers remain and any lock it acquired was released (true immediately if it never
            //    acquired the lock). Otherwise retain the database until it becomes true.
            if (runner == null || runner.fullyStopped()) {
                try {
                    db.close();
                } catch (RuntimeException closeFailure) {
                    t.addSuppressed(closeFailure);
                }
            } else {
                LOG.error("Durable runtime failed to start; runner workers still alive, database retained until they exit");
                closeWhenRunnerStopped(runner, db, null);
            }
            throw t;
        }
    }

    /**
     * Daemon that closes {@code db} only once {@code runner.fullyStopped()}. If interrupted it leaves the
     * database open rather than risk racing a live worker.
     */
    private static void closeWhenRunnerStopped(OperationRunner runner, StateDatabase db, Runnable onClosed) {
        Thread closer = new Thread(() -> {
            while (!runner.fullyStopped()) {
                try {
                    Thread.sleep(250);
                } catch (InterruptedException e) {
                    return;
                }
            }
            try {
                db.close();
                if (onClosed != null) {
                    onClosed.run();
                }
            } catch (RuntimeException e) {
                LOG.warn("Failed to close state database: {}", e.getClass().getSimpleName());
            }
        }, "vesta-runtime-db-closer");
        closer.setDaemon(true);
        closer.start();
    }

    /**
     * Fails closed ({@code state_dir_linked}) if the raw selected state path or any existing ancestor is a
     * symbolic link, junction or other reparse point/special file. Checked without following links.
     * Note: platforms whose system paths are links (e.g. macOS {@code /var}) need a non-linked location.
     */
    static void rejectLinkedPath(Path selected) {
        Path p = selected.toAbsolutePath().normalize();
        for (Path cur = p; cur != null; cur = cur.getParent()) {
            if (!Files.exists(cur, LinkOption.NOFOLLOW_LINKS) && !Files.isSymbolicLink(cur)) {
                continue;
            }
            BasicFileAttributes attrs;
            try {
                attrs = Files.readAttributes(cur, BasicFileAttributes.class, LinkOption.NOFOLLOW_LINKS);
            } catch (IOException e) {
                throw new StateException("state_dir_linked", "Cannot inspect state directory path");
            }
            boolean root = cur.getParent() == null;
            if (attrs.isSymbolicLink() || (attrs.isOther() && !root) || !attrs.isDirectory()) {
                throw new StateException("state_dir_linked",
                        "State directory or an ancestor is a link, junction, reparse point or not a directory");
            }
        }
    }

    public StateDirectory stateDirectory() {
        return stateDirectory;
    }

    public StateDatabase database() {
        return database;
    }

    public AuditLog audit() {
        return audit;
    }

    public EventLog events() {
        return events;
    }

    public OperationService operations() {
        return operations;
    }

    public IdentityService identities() {
        return identities;
    }

    public AuthorizationService authorization() {
        return authorization;
    }

    public OperationBindings bindings() {
        return bindings;
    }

    public OperationSubmissionService submissions() {
        return submissions;
    }

    /** Canonical confirmation gateway (policy-checked, trusted generation revision). */
    public PolicyConfirmationService confirmations() {
        return confirmations;
    }

    /** Underlying token store; prefer {@link #confirmations()}. */
    public ConfirmationService confirmationTokens() {
        return confirmationTokens;
    }

    public LifecycleBindings.GenerationRevision generationRevision() {
        return generationRevision;
    }

    public OperationRunner runner() {
        return runner;
    }

    public MinecraftServerProcess game() {
        return game;
    }

    /** True once the database was closed (after every runner worker exited). */
    public boolean databaseClosed() {
        return databaseClosed;
    }

    /**
     * Bounded shutdown: stops the runner (grace, interrupt, grace). Closes the database only when every
     * worker has exited and runner ownership was released, and returns true. Otherwise returns false and
     * retains the database (never racing a close with a handler that did not cooperate); a daemon closes it
     * once the workers exit. Idempotent. Does not stop the game.
     */
    public synchronized boolean shutdown() {
        if (shutdownRequested) {
            return databaseClosed;
        }
        shutdownRequested = true;
        try {
            runner.close();
        } catch (RuntimeException e) {
            LOG.warn("Operation runner shutdown failed: {}", e.getClass().getSimpleName());
        }
        if (runner.fullyStopped()) {
            closeDatabase();
            return databaseClosed;
        }
        LOG.warn("Operation handlers still running; durable state retained until they exit");
        closeWhenRunnerStopped(runner, database, () -> databaseClosed = true);
        return false;
    }

    private void closeDatabase() {
        try {
            database.close();
            databaseClosed = true;
        } catch (RuntimeException e) {
            LOG.warn("Failed to close state database: {}", e.getClass().getSimpleName());
        }
    }

    @Override
    public void close() {
        shutdown();
    }
}
