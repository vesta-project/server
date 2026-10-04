package com.vesta.wrapper.operations.lifecycle;

import com.vesta.wrapper.authorization.Capability;
import com.vesta.wrapper.operations.HandlerContract;
import com.vesta.wrapper.operations.Operation;
import com.vesta.wrapper.operations.OperationBinding;
import com.vesta.wrapper.operations.OperationContext;
import com.vesta.wrapper.operations.OperationFailure;
import com.vesta.wrapper.operations.OperationHandler;
import com.vesta.wrapper.operations.PolicyConfirmationService;
import com.vesta.wrapper.runtime.MinecraftServerProcess;
import com.vesta.wrapper.runtime.MinecraftServerProcess.LifecycleException;

import java.sql.Connection;
import java.time.Duration;
import java.util.List;
import java.util.Map;
import java.util.Objects;
import java.util.Set;

/**
 * Trusted typed bindings for game lifecycle operations of the wrapper-owned Minecraft process.
 *
 * <ul>
 *   <li>{@value #START}: input exactly {@code {}}; target {@code game}; requires {@code lifecycle.start};
 *       non-idempotent, no confirmation. An already running game is reported without any effect.</li>
 *   <li>{@value #STOP}: input {@code {"expected_generation":"gen_..."}}; target
 *       {@code game:generation:<id>}; requires {@code lifecycle.stop}; idempotent because it is bound to one
 *       generation (a replay never stops a replacement), no confirmation, never kills.</li>
 *   <li>{@value #FORCE_KILL}: same input/target form; requires {@code lifecycle.force_kill}; non-idempotent,
 *       exact confirmation required.</li>
 * </ul>
 * All hold the single lock resource {@value #LOCK}. No restart operation exists yet.
 *
 * <p>Cancellation: each handler honors cancel/shutdown up to {@link OperationContext#beginExternalEffect()}
 * (called under the process monitor immediately before the effect). After the effect, cancel requests are
 * not honored ({@code cancellableAfterEffect=false}): a stop line or kill signal cannot be undone.
 *
 * <p>Scope: the game remains wrapper-owned (a child of this JVM); there is no independent supervisor.
 */
public final class LifecycleBindings {
    public static final String START = "game.start";
    public static final String STOP = "game.stop";
    public static final String FORCE_KILL = "game.force_kill";
    /** Shared lock serializing every lifecycle mutation of the game generation. */
    public static final String LOCK = "game.generation";
    public static final String FIELD_EXPECTED_GENERATION = "expected_generation";
    public static final String REVISION_PREFIX = "game-generation:";
    /** How long a force kill waits for the process to exit after the signal. */
    public static final Duration KILL_WAIT = Duration.ofSeconds(5);

    static final HandlerContract START_CONTRACT = new HandlerContract(false, false, false);
    static final HandlerContract STOP_CONTRACT = new HandlerContract(true, false, false);
    static final HandlerContract KILL_CONTRACT = new HandlerContract(false, true, false);

    private LifecycleBindings() {
    }

    /** Typed input of start (no fields). */
    public record StartInput() {
    }

    /** Typed input of stop/force-kill: the exact generation the requester observed. */
    public record GenerationInput(String expectedGeneration) {
        public GenerationInput {
            if (expectedGeneration == null
                    || !MinecraftServerProcess.GENERATION_ID.matcher(expectedGeneration).matches()) {
                throw new IllegalArgumentException("malformed generation id");
            }
        }
    }

    /** Creates the three bindings for {@code process}. */
    public static List<OperationBinding<?>> create(MinecraftServerProcess process) {
        Objects.requireNonNull(process, "process");
        return List.of(
                OperationBinding.of(new StartHandler(process), new StartDerivation()),
                OperationBinding.of(new StopHandler(process), new GenerationDerivation(Capability.LIFECYCLE_STOP)),
                OperationBinding.of(new KillHandler(process), new GenerationDerivation(Capability.LIFECYCLE_FORCE_KILL)));
    }

    // ------------------------------------------------------------------ parsing

    static StartInput parseStart(String json) throws OperationFailure {
        StrictJson.parseObject(json, Set.of());
        return new StartInput();
    }

    static GenerationInput parseGeneration(String json) throws OperationFailure {
        Map<String, String> fields = StrictJson.parseObject(json, Set.of(FIELD_EXPECTED_GENERATION));
        String gen = fields.get(FIELD_EXPECTED_GENERATION);
        if (gen == null) {
            throw new OperationFailure("invalid_input", "expected_generation is required");
        }
        try {
            return new GenerationInput(gen);
        } catch (IllegalArgumentException e) {
            throw new OperationFailure("invalid_input", "expected_generation is malformed");
        }
    }

    static String canonical(GenerationInput input) {
        // The id alphabet needs no JSON escaping.
        return "{\"" + FIELD_EXPECTED_GENERATION + "\":\"" + input.expectedGeneration() + "\"}";
    }

    static String target(GenerationInput input) {
        return "game:generation:" + input.expectedGeneration();
    }

    private static OperationFailure known(LifecycleException e) {
        return new OperationFailure(e.getCode(), e.getMessage());
    }

    // ------------------------------------------------------------------ derivations

    private static final class StartDerivation implements OperationBinding.Derivation<StartInput> {
        @Override
        public String canonicalInput(StartInput input) {
            return "{}";
        }

        @Override
        public String target(StartInput input) {
            return "game";
        }

        @Override
        public List<OperationBinding.Requirement> requirements(StartInput input) {
            return List.of(new OperationBinding.Requirement(Capability.LIFECYCLE_START, ""));
        }
    }

    private static final class GenerationDerivation implements OperationBinding.Derivation<GenerationInput> {
        private final Capability capability;

        GenerationDerivation(Capability capability) {
            this.capability = capability;
        }

        @Override
        public String canonicalInput(GenerationInput input) {
            return canonical(input);
        }

        @Override
        public String target(GenerationInput input) {
            return LifecycleBindings.target(input);
        }

        @Override
        public List<OperationBinding.Requirement> requirements(GenerationInput input) {
            return List.of(new OperationBinding.Requirement(capability, ""));
        }
    }

    // ------------------------------------------------------------------ handlers

    static final class StartHandler implements OperationHandler<StartInput> {
        private final MinecraftServerProcess process;

        StartHandler(MinecraftServerProcess process) {
            this.process = process;
        }

        @Override
        public String type() {
            return START;
        }

        @Override
        public HandlerContract contract() {
            return START_CONTRACT;
        }

        @Override
        public StartInput parseInput(String canonicalJson) throws OperationFailure {
            return parseStart(canonicalJson);
        }

        @Override
        public Set<String> requiredResources(StartInput input) {
            return Set.of(LOCK);
        }

        @Override
        public String execute(StartInput input, OperationContext context) throws OperationFailure {
            MinecraftServerProcess.StartResult r;
            try {
                r = process.startGeneration(context::beginExternalEffect);
            } catch (LifecycleException e) {
                throw known(e);
            }
            return "{\"outcome\":\"" + (r.launched() ? "started" : "already_running")
                    + "\",\"generation\":\"" + r.generationId() + "\"}";
        }
    }

    static final class StopHandler implements OperationHandler<GenerationInput> {
        private final MinecraftServerProcess process;

        StopHandler(MinecraftServerProcess process) {
            this.process = process;
        }

        @Override
        public String type() {
            return STOP;
        }

        @Override
        public HandlerContract contract() {
            return STOP_CONTRACT;
        }

        @Override
        public GenerationInput parseInput(String canonicalJson) throws OperationFailure {
            return parseGeneration(canonicalJson);
        }

        @Override
        public Set<String> requiredResources(GenerationInput input) {
            return Set.of(LOCK);
        }

        /**
         * Interruption while waiting propagates as {@link InterruptedException}: during runner shutdown the
         * (idempotent, generation-bound) operation is re-queued; the process stays STOPPING.
         */
        @Override
        public String execute(GenerationInput input, OperationContext context)
                throws OperationFailure, InterruptedException {
            MinecraftServerProcess.StopResult r;
            try {
                r = process.stopGeneration(input.expectedGeneration(), context::beginExternalEffect,
                        process.stopTimeout());
            } catch (LifecycleException e) {
                throw known(e);
            }
            if ("not_running".equals(r.outcome())) {
                return "{\"outcome\":\"not_running\"}";
            }
            return "{\"outcome\":\"stopped\",\"generation\":\"" + r.generationId() + "\""
                    + (r.exitCode() == null ? "" : ",\"exit_code\":" + r.exitCode()) + "}";
        }
    }

    static final class KillHandler implements OperationHandler<GenerationInput> {
        private final MinecraftServerProcess process;

        KillHandler(MinecraftServerProcess process) {
            this.process = process;
        }

        @Override
        public String type() {
            return FORCE_KILL;
        }

        @Override
        public HandlerContract contract() {
            return KILL_CONTRACT;
        }

        @Override
        public GenerationInput parseInput(String canonicalJson) throws OperationFailure {
            return parseGeneration(canonicalJson);
        }

        @Override
        public Set<String> requiredResources(GenerationInput input) {
            return Set.of(LOCK);
        }

        /**
         * Known no-effect failures ({@code stale_generation}, {@code not_running}) fail the operation. Once the
         * signal was sent, an exit not observed within {@link #KILL_WAIT} ({@code kill_unconfirmed}) or an
         * interruption while waiting is an UNKNOWN outcome: it is thrown as a non-{@link OperationFailure} so
         * the runner records the started non-idempotent effect as recovery_required (locks retained, never
         * replayed) instead of a known failure.
         */
        @Override
        public String execute(GenerationInput input, OperationContext context)
                throws OperationFailure, InterruptedException {
            MinecraftServerProcess.KillResult r;
            try {
                r = process.killGeneration(input.expectedGeneration(), context::beginExternalEffect, KILL_WAIT);
            } catch (LifecycleException e) {
                if (LifecycleException.KILL_UNCONFIRMED.equals(e.getCode())) {
                    throw new KillOutcomeUnknown();
                }
                throw known(e);
            }
            return "{\"outcome\":\"killed\",\"generation\":\"" + r.generationId() + "\""
                    + (r.exitCode() == null ? "" : ",\"exit_code\":" + r.exitCode()) + "}";
        }
    }

    /** Kill signal sent but process exit not established: unknown outcome (recovery_required). */
    static final class KillOutcomeUnknown extends IllegalStateException {
        KillOutcomeUnknown() {
            super("Kill signal sent but process exit was not observed");
        }
    }

    // ------------------------------------------------------------------ confirmation revision

    /**
     * Trusted source revision for the confirmation gateway: the live game generation read lock-free from
     * the process (no database access). For generation-bound operations it returns a revision only when the
     * operation's canonical {@code expected_generation} is the live generation, otherwise null (stale target
     * refused). Other operation types get the live generation (or {@code none}).
     *
     * <p>Equality at issue/confirm time is advisory; the execution-time guarantee is the handler's atomic
     * generation check under the process monitor.
     */
    public static final class GenerationRevision implements PolicyConfirmationService.SourceRevisionProvider {
        private final MinecraftServerProcess process;

        public GenerationRevision(MinecraftServerProcess process) {
            this.process = Objects.requireNonNull(process, "process");
        }

        /** Current live revision string, {@code game-generation:none} when nothing is running. */
        public String current() {
            String live = process.currentGenerationId();
            return REVISION_PREFIX + (live == null ? "none" : live);
        }

        @Override
        public String sourceRevision(Connection c, Operation operation, String canonicalInput) {
            if (operation == null) {
                return null;
            }
            String live = process.currentGenerationId();
            if (STOP.equals(operation.type()) || FORCE_KILL.equals(operation.type())) {
                GenerationInput input;
                try {
                    input = parseGeneration(canonicalInput);
                } catch (OperationFailure e) {
                    return null;
                }
                if (live == null || !live.equals(input.expectedGeneration())) {
                    return null;
                }
                return REVISION_PREFIX + live;
            }
            return REVISION_PREFIX + (live == null ? "none" : live);
        }
    }
}
