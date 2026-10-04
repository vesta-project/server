package com.vesta.wrapper.operations;

import com.google.gson.Gson;
import com.vesta.wrapper.audit.AuditActor;
import com.vesta.wrapper.audit.AuditLog;
import com.vesta.wrapper.audit.AuditRecord;
import com.vesta.wrapper.state.EventLog;
import com.vesta.wrapper.state.StateDatabase;
import com.vesta.wrapper.state.StateValues;

import java.sql.Connection;
import java.sql.PreparedStatement;
import java.sql.ResultSet;
import java.sql.SQLException;
import java.time.Instant;
import java.util.ArrayList;
import java.util.EnumSet;
import java.util.HashSet;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import java.util.Objects;
import java.util.Optional;
import java.util.Set;
import java.util.UUID;

/**
 * Durable operation repository and state machine.
 *
 * <p>Persists the bounded canonical request input (schema v2+) so a registered handler can
 * execute it later; rows created before v2 have no input and are never executed. Every state
 * transition commits atomically together with its audit record and event; if auditing fails the
 * transition does not happen.
 *
 * <p>Ownership: {@link #claimNext(String)} stamps an owner token on the claimed row, and the
 * token-taking lifecycle methods only mutate a row whose token matches exactly. The token-less
 * overloads are equivalent to passing {@code null} and therefore only act on rows claimed via the
 * token-less {@link #claimNext()}.
 */
public final class OperationService {
    public static final int MAX_RESULT_CHARS = 16_384;
    public static final int MAX_ERROR_CHARS = 1_024;
    public static final int MAX_PAGE = 200;
    public static final String INPUT_UNAVAILABLE = "input_unavailable";
    static final String ENGINE_SOURCE = "operation-engine";
    private static final Gson GSON = new Gson();
    private static final String COLUMNS = "id, seq, server_id, actor_principal_id, device_id, source, type, target, "
            + "payload_hash, idempotency_key, state, requires_confirmation, progress_percent, progress_message, "
            + "cancel_requested, external_effect_started, external_effect_idempotent, result_json, error_code, "
            + "error_message, created_at, updated_at, finished_at";

    private final StateDatabase db;
    private final AuditLog audit;
    private final EventLog events;

    public OperationService(StateDatabase db, AuditLog audit, EventLog events) {
        this.db = db;
        this.audit = audit;
        this.events = events;
    }

    public StateDatabase database() {
        return db;
    }

    // ------------------------------------------------------------------ submission

    /**
     * Unguarded submission for trusted in-process callers that already authorized the request.
     * Remote/user submissions must go through {@link OperationSubmissionService}.
     */
    public SubmitResult submit(OperationRequest r) {
        return submitGuarded(r, null);
    }

    /** Authorization guard evaluated inside the submit transaction; returns null to allow or a deny reason. */
    @FunctionalInterface
    interface SubmitGuard {
        String denyReason(Connection c) throws SQLException;
    }

    private record SubmitOutcome(SubmitResult result, String rejectionCode, String reason) {
    }

    /**
     * Submits in one write transaction: {@code guard} (if any) is evaluated first, then idempotency and the
     * insert with its audit/event. A denial or idempotency conflict appends a rejection audit in that same
     * transaction (no operation row) and is thrown after commit; if that audit fails nothing commits.
     */
    SubmitResult submitGuarded(OperationRequest r, SubmitGuard guard) {
        SubmitOutcome outcome = db.write(c -> {
            if (guard != null) {
                String denial = guard.denyReason(c);
                if (denial != null) {
                    rejectSubmit(c, r, denial);
                    return new SubmitOutcome(null, OperationException.AUTHORIZATION_DENIED, denial);
                }
            }
            SubmitResult result = submitBody(c, r);
            if (result == null) {
                rejectSubmit(c, r, OperationException.IDEMPOTENCY_CONFLICT);
                return new SubmitOutcome(null, OperationException.IDEMPOTENCY_CONFLICT, null);
            }
            return new SubmitOutcome(result, null, null);
        });
        if (OperationException.AUTHORIZATION_DENIED.equals(outcome.rejectionCode())) {
            throw new OperationException(OperationException.AUTHORIZATION_DENIED,
                    "Operation not authorized: " + outcome.reason());
        }
        if (OperationException.IDEMPOTENCY_CONFLICT.equals(outcome.rejectionCode())) {
            throw new OperationException(OperationException.IDEMPOTENCY_CONFLICT,
                    "Idempotency key was already used for a different request");
        }
        return outcome.result();
    }

    private void rejectSubmit(Connection c, OperationRequest r, String reason) throws SQLException {
        audit.append(c, new AuditRecord(r.actor(), "operation.submit", r.target(), "rejected", null,
                Map.of("reason", StateValues.bounded(reason, 64), "type", r.type())));
    }

    /** Inserts the operation; returns null on an idempotency conflict (caller audits and rejects). */
    private SubmitResult submitBody(Connection c, OperationRequest r) throws SQLException {
        String payloadHash = r.payloadHash();
        if (r.idempotencyKey() != null) {
            Optional<Operation> existing = findByIdempotencyKey(c, r.actor().principalId(), r.idempotencyKey());
            if (existing.isPresent()) {
                Operation op = existing.get();
                return sameBinding(op, r, payloadHash) ? new SubmitResult(op, false) : null;
            }
        }
        String id = UUID.randomUUID().toString();
        long now = db.clock().millis();
        OperationState initial = r.requiresConfirmation() ? OperationState.AWAITING_CONFIRMATION : OperationState.QUEUED;
        try (PreparedStatement ps = c.prepareStatement(
                "INSERT INTO operations (id, server_id, actor_principal_id, device_id, source, type, target, "
                        + "payload_hash, idempotency_key, state, requires_confirmation, external_effect_idempotent, "
                        + "created_at, updated_at, input_json, input_status) "
                        + "VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,'available')")) {
            ps.setString(1, id);
            ps.setString(2, db.serverId());
            ps.setString(3, r.actor().principalId());
            ps.setString(4, r.actor().deviceId());
            ps.setString(5, r.actor().source());
            ps.setString(6, r.type());
            ps.setString(7, r.target());
            ps.setString(8, payloadHash);
            ps.setString(9, r.idempotencyKey());
            ps.setString(10, initial.wireName());
            ps.setInt(11, r.requiresConfirmation() ? 1 : 0);
            ps.setInt(12, r.externalEffectIdempotent() ? 1 : 0);
            ps.setLong(13, now);
            ps.setLong(14, now);
            ps.setString(15, r.canonicalPayload());
            ps.executeUpdate();
        }
        try (PreparedStatement ps = c.prepareStatement(
                "INSERT INTO operation_resources (operation_id, resource) VALUES (?,?)")) {
            for (String resource : r.lockResources()) {
                ps.setString(1, id);
                ps.setString(2, resource);
                ps.executeUpdate();
            }
        }
        Map<String, String> meta = new LinkedHashMap<>();
        meta.put("type", r.type());
        meta.put("state", initial.wireName());
        meta.put("payloadHash", payloadHash);
        audit.append(c, new AuditRecord(r.actor(), "operation.submit", r.target(), "accepted", id, meta));
        events.append(c, "operation.created", id, stateJson(id, initial, 0));
        return new SubmitResult(load(c, id).orElseThrow(), true);
    }

    // ------------------------------------------------------------------ queries

    public Optional<Operation> get(String id) {
        return db.read(c -> load(c, id));
    }

    /**
     * Stored input for an operation. Status is {@link OperationInput.InputStatus#UNAVAILABLE_LEGACY}
     * for pre-v2 rows (and also when the stored JSON no longer matches the bound payload hash).
     */
    public Optional<OperationInput> input(String id) {
        return db.read(c -> loadInput(c, id));
    }

    /** Operations with seq greater than {@code afterSeq}, oldest first, bounded page. */
    public List<Operation> list(long afterSeq, int limit) {
        int bounded = Math.max(1, Math.min(MAX_PAGE, limit));
        return db.read(c -> {
            List<String> ids = new ArrayList<>();
            try (PreparedStatement ps = c.prepareStatement("SELECT id FROM operations WHERE seq > ? ORDER BY seq LIMIT ?")) {
                ps.setLong(1, afterSeq);
                ps.setInt(2, bounded);
                try (ResultSet rs = ps.executeQuery()) {
                    while (rs.next()) {
                        ids.add(rs.getString(1));
                    }
                }
            }
            List<Operation> out = new ArrayList<>(ids.size());
            for (String id : ids) {
                out.add(load(c, id).orElseThrow());
            }
            return List.copyOf(out);
        });
    }

    /**
     * Drops stored inputs of terminal (succeeded/failed/cancelled) operations finished before
     * {@code finishedBefore}. Never touches non-terminal or recovery_required inputs. When rows are
     * purged, a system audit record (count and cutoff only, no payload) is appended in the same
     * transaction; if auditing fails nothing is erased.
     */
    public int purgeTerminalInputs(Instant finishedBefore) {
        Objects.requireNonNull(finishedBefore, "finishedBefore");
        return db.write(c -> {
            int purged;
            try (PreparedStatement ps = c.prepareStatement(
                    "UPDATE operations SET input_json = NULL, input_status = 'purged' WHERE input_status = 'available' "
                            + "AND state IN ('succeeded','failed','cancelled') AND finished_at IS NOT NULL AND finished_at < ?")) {
                ps.setLong(1, finishedBefore.toEpochMilli());
                purged = ps.executeUpdate();
            }
            if (purged > 0) {
                audit.append(c, new AuditRecord(AuditActor.system(ENGINE_SOURCE), "operation.inputs_purged",
                        "operations", "ok", null,
                        Map.of("count", Integer.toString(purged), "finishedBefore", finishedBefore.toString())));
            }
            return purged;
        });
    }

    // ------------------------------------------------------------------ worker lifecycle

    /** Claims without an owner token (manual/administrative use). See {@link #claimNext(String)}. */
    public Optional<Operation> claimNext() {
        return claimNextInternal(null).map(ClaimedOperation::operation);
    }

    /**
     * Claims the oldest runnable queued operation (FIFO) with an available input, atomically
     * acquiring all of its resource locks in sorted order or none, and stamps {@code ownerToken}.
     * A queued operation blocked by held locks reserves its resources so later conflicting
     * operations cannot overtake it. Moves the claimed operation to validating.
     */
    public Optional<ClaimedOperation> claimNext(String ownerToken) {
        StateValues.requireText("ownerToken", ownerToken, 128);
        return claimNextInternal(ownerToken);
    }

    private Optional<ClaimedOperation> claimNextInternal(String ownerToken) {
        return db.write(c -> {
            Set<String> unavailable = new HashSet<>();
            try (PreparedStatement ps = c.prepareStatement("SELECT resource FROM resource_locks");
                 ResultSet rs = ps.executeQuery()) {
                while (rs.next()) {
                    unavailable.add(rs.getString(1));
                }
            }
            List<String> queued = new ArrayList<>();
            try (PreparedStatement ps = c.prepareStatement(
                    "SELECT id FROM operations WHERE state = 'queued' ORDER BY seq");
                 ResultSet rs = ps.executeQuery()) {
                while (rs.next()) {
                    queued.add(rs.getString(1));
                }
            }
            for (String id : queued) {
                boolean inputAvailable = loadInput(c, id)
                        .map(i -> i.status() == OperationInput.InputStatus.AVAILABLE).orElse(false);
                if (!inputAvailable) {
                    // Never execute, never leave stuck: fail explicitly and audibly (nothing has started).
                    Operation before = load(c, id).orElseThrow();
                    update(c, id, "error_code = ?, error_message = ?", INPUT_UNAVAILABLE,
                            "Stored operation input is unavailable; it will not be executed");
                    finish(c, id, OperationState.FAILED);
                    record(c, AuditActor.system(ENGINE_SOURCE), "operation.input_unavailable",
                            load(c, id).orElseThrow(), before.state(), Map.of());
                    continue;
                }
                List<String> resources = resources(c, id);
                boolean free = resources.stream().noneMatch(unavailable::contains);
                if (!free) {
                    unavailable.addAll(resources);
                    continue;
                }
                long now = db.clock().millis();
                try (PreparedStatement ps = c.prepareStatement(
                        "INSERT INTO resource_locks (resource, operation_id, acquired_at) VALUES (?,?,?)")) {
                    for (String resource : resources) { // already sorted: deterministic order
                        ps.setString(1, resource);
                        ps.setString(2, id);
                        ps.setLong(3, now);
                        ps.executeUpdate();
                    }
                }
                update(c, id, "state = ?, owner_token = ?", OperationState.VALIDATING.wireName(), ownerToken);
                Operation op = load(c, id).orElseThrow();
                record(c, AuditActor.system(ENGINE_SOURCE), "operation.claim", op, OperationState.QUEUED, Map.of());
                return Optional.of(new ClaimedOperation(op, ownerToken));
            }
            return Optional.empty();
        });
    }

    public Operation markRunning(String id) {
        return markRunning(id, null);
    }

    public Operation markRunning(String id, String ownerToken) {
        return transition(id, ownerToken, AuditActor.system(ENGINE_SOURCE), "operation.run",
                EnumSet.of(OperationState.VALIDATING), (c, op) -> setState(c, id, OperationState.RUNNING));
    }

    public Operation markExternalEffectStarted(String id) {
        return markExternalEffectStarted(id, null);
    }

    /** Durably records that a non-replayable external effect may now happen. Call BEFORE the effect. */
    public Operation markExternalEffectStarted(String id, String ownerToken) {
        return transition(id, ownerToken, AuditActor.system(ENGINE_SOURCE), "operation.effect_started",
                EnumSet.of(OperationState.RUNNING), (c, op) -> update(c, id, "external_effect_started = 1"));
    }

    public Operation reportProgress(String id, int percent, String message) {
        return reportProgress(id, null, percent, message);
    }

    /** Progress is persisted and emitted as an event; it is not an audited state transition. */
    public Operation reportProgress(String id, String ownerToken, int percent, String message) {
        if (percent < 0 || percent > 100) {
            throw new OperationException(OperationException.INVALID_REQUEST, "progress must be 0..100");
        }
        String msg = StateValues.bounded(message, 256);
        return db.write(c -> {
            Operation op = require(c, id);
            requireOwner(c, op, ownerToken);
            if (!op.state().active()) {
                throw invalid(op, "report progress");
            }
            update(c, id, "progress_percent = ?, progress_message = ?", percent, msg);
            events.append(c, "operation.progress", id, stateJson(id, op.state(), percent));
            return load(c, id).orElseThrow();
        });
    }

    public Operation succeed(String id, String resultJson) {
        return succeed(id, null, resultJson);
    }

    public Operation succeed(String id, String ownerToken, String resultJson) {
        String result = boundedOrReject(resultJson, MAX_RESULT_CHARS, "result");
        return transition(id, ownerToken, AuditActor.system(ENGINE_SOURCE), "operation.succeed",
                EnumSet.of(OperationState.RUNNING, OperationState.CANCELLING), (c, op) -> {
                    update(c, id, "progress_percent = 100, result_json = ?", result);
                    finish(c, id, OperationState.SUCCEEDED);
                });
    }

    public Operation fail(String id, String errorCode, String errorMessage) {
        return fail(id, null, errorCode, errorMessage);
    }

    public Operation fail(String id, String ownerToken, String errorCode, String errorMessage) {
        String code = StateValues.requireText("errorCode", errorCode, 64);
        String message = StateValues.bounded(errorMessage, MAX_ERROR_CHARS);
        return transition(id, ownerToken, AuditActor.system(ENGINE_SOURCE), "operation.fail",
                EnumSet.of(OperationState.VALIDATING, OperationState.RUNNING, OperationState.CANCELLING), (c, op) -> {
                    update(c, id, "error_code = ?, error_message = ?", code, message);
                    finish(c, id, OperationState.FAILED);
                });
    }

    /**
     * Records that the outcome of a started external effect is unknown. Locks are retained until an
     * owner resolves the operation via {@link #resolveRecovery}; it is never replayed.
     */
    public Operation markRecoveryRequired(String id, String ownerToken, String reason) {
        String code = StateValues.requireText("reason", reason, 64);
        return transition(id, ownerToken, AuditActor.system(ENGINE_SOURCE), "operation.recovery_required",
                EnumSet.of(OperationState.VALIDATING, OperationState.RUNNING, OperationState.CANCELLING), (c, op) -> {
                    update(c, id, "error_code = ?, owner_token = NULL", code);
                    setState(c, id, OperationState.RECOVERY_REQUIRED);
                });
    }

    /**
     * Releases a claimed operation when the runner shuts down at a safe point: not-started or
     * idempotent effects are re-queued (locks and owner released); a started non-idempotent effect
     * becomes recovery_required.
     */
    public Operation suspendForShutdown(String id, String ownerToken) {
        return transition(id, ownerToken, AuditActor.system(ENGINE_SOURCE), "operation.suspend",
                EnumSet.of(OperationState.VALIDATING, OperationState.RUNNING, OperationState.CANCELLING),
                this::reconcile);
    }

    /**
     * Requests cancellation. Queued/awaiting-confirmation operations are cancelled immediately;
     * claimed operations move to cancelling and the worker must stop at a safe point and call
     * {@link #acknowledgeCancelled}. Repeating a request on a cancelling operation is a no-op.
     */
    public Operation requestCancel(AuditActor actor, String id) {
        Operation current = get(id).orElseThrow(() -> notFound(id));
        if (current.state() == OperationState.CANCELLING) {
            return current;
        }
        return db.write(c -> transition(c, id, actor, "operation.cancel_request",
                EnumSet.of(OperationState.QUEUED, OperationState.AWAITING_CONFIRMATION,
                        OperationState.VALIDATING, OperationState.RUNNING), (cc, op) -> {
                    update(cc, id, "cancel_requested = 1");
                    if (op.state().active()) {
                        setState(cc, id, OperationState.CANCELLING);
                    } else {
                        finish(cc, id, OperationState.CANCELLED);
                    }
                }));
    }

    public Operation acknowledgeCancelled(String id) {
        return acknowledgeCancelled(id, null);
    }

    public Operation acknowledgeCancelled(String id, String ownerToken) {
        return transition(id, ownerToken, AuditActor.system(ENGINE_SOURCE), "operation.cancelled",
                EnumSet.of(OperationState.CANCELLING), (c, op) -> finish(c, id, OperationState.CANCELLED));
    }

    /**
     * Startup reconciliation. Must only run when no runner is active on this database.
     * <ul>
     *   <li>Claimed operations: non-idempotent operations whose external effect may have started become
     *       recovery_required (locks retained, never replayed); others are re-queued (locks/owner
     *       released) or, if cancelling, cancelled.</li>
     *   <li>Non-terminal operations without an available input (pre-v2 rows): failed with
     *       {@code input_unavailable}, unless a non-idempotent effect started (recovery_required).</li>
     * </ul>
     */
    public List<Operation> recoverAfterRestart() {
        return db.write(c -> {
            List<String> ids = new ArrayList<>();
            try (PreparedStatement ps = c.prepareStatement(
                    "SELECT id FROM operations WHERE state IN ('validating','running','cancelling') "
                            + "OR (input_status <> 'available' AND state IN ('queued','awaiting_confirmation')) ORDER BY seq");
                 ResultSet rs = ps.executeQuery()) {
                while (rs.next()) {
                    ids.add(rs.getString(1));
                }
            }
            AuditActor engine = AuditActor.system(ENGINE_SOURCE);
            List<Operation> out = new ArrayList<>();
            for (String id : ids) {
                Operation op = load(c, id).orElseThrow();
                boolean inputAvailable = loadInput(c, id).map(i -> i.status() == OperationInput.InputStatus.AVAILABLE)
                        .orElse(false);
                if (!inputAvailable && !(op.externalEffectStarted() && !op.externalEffectIdempotent())) {
                    update(c, id, "error_code = ?, error_message = ?, owner_token = NULL", INPUT_UNAVAILABLE,
                            "Stored operation input is unavailable; it will not be executed");
                    finish(c, id, OperationState.FAILED);
                } else {
                    reconcile(c, op);
                }
                Operation after = load(c, id).orElseThrow();
                record(c, engine, "operation.recover", after, op.state(), Map.of());
                out.add(after);
            }
            return List.copyOf(out);
        });
    }

    /** Owner decision for a recovery_required operation. Authorization is the caller's responsibility. */
    public Operation resolveRecovery(AuditActor owner, String id, OperationState outcome, String note) {
        if (!outcome.terminal()) {
            throw new OperationException(OperationException.INVALID_REQUEST, "outcome must be terminal");
        }
        String safeNote = StateValues.bounded(note, 256);
        return db.write(c -> transition(c, id, owner, "operation.resolve_recovery",
                EnumSet.of(OperationState.RECOVERY_REQUIRED), (cc, op) -> {
                    if (outcome == OperationState.FAILED) {
                        update(cc, id, "error_code = 'recovery_resolved_failed', error_message = ?", safeNote);
                    }
                    finish(cc, id, outcome);
                }));
    }

    // ------------------------------------------------------------------ package-level helpers

    @FunctionalInterface
    interface Mutation {
        void apply(Connection c, Operation current) throws SQLException;
    }

    /** Owner-checked transition: the row's owner token must equal {@code ownerToken} (both may be null). */
    Operation transition(String id, String ownerToken, AuditActor actor, String action, Set<OperationState> from,
                         Mutation mutation) {
        return db.write(c -> {
            Operation op = require(c, id);
            requireOwner(c, op, ownerToken);
            return transition(c, id, actor, action, from, mutation);
        });
    }

    /** Unowned transition used for actor requests (cancel, recovery resolution, confirmation). */
    Operation transition(Connection c, String id, AuditActor actor, String action, Set<OperationState> from,
                         Mutation mutation) throws SQLException {
        Operation op = require(c, id);
        if (!from.contains(op.state())) {
            throw invalid(op, action);
        }
        mutation.apply(c, op);
        Operation after = load(c, id).orElseThrow();
        record(c, actor, action, after, op.state(), Map.of());
        return after;
    }

    void record(Connection c, AuditActor actor, String action, Operation after, OperationState before,
                Map<String, String> extra) throws SQLException {
        Map<String, String> meta = new LinkedHashMap<>(extra);
        meta.put("type", after.type());
        meta.put("from", before.wireName());
        meta.put("to", after.state().wireName());
        audit.append(c, new AuditRecord(actor, action, after.target(), "ok", after.id(), meta));
        events.append(c, "operation.state", after.id(), stateJson(after.id(), after.state(), after.progressPercent()));
    }

    Operation require(Connection c, String id) throws SQLException {
        return load(c, id).orElseThrow(() -> notFound(id));
    }

    void setState(Connection c, String id, OperationState state) throws SQLException {
        update(c, id, "state = ?", state.wireName());
    }

    Optional<Operation> load(Connection c, String id) throws SQLException {
        try (PreparedStatement ps = c.prepareStatement("SELECT " + COLUMNS + " FROM operations WHERE id = ?")) {
            ps.setString(1, id);
            try (ResultSet rs = ps.executeQuery()) {
                if (!rs.next()) {
                    return Optional.empty();
                }
                return Optional.of(map(rs, resources(c, id)));
            }
        }
    }

    // ------------------------------------------------------------------ internals

    /** Shared crash/shutdown reconciliation for one claimed operation. */
    private void reconcile(Connection c, Operation op) throws SQLException {
        String id = op.id();
        if (op.externalEffectStarted() && !op.externalEffectIdempotent()) {
            update(c, id, "owner_token = NULL, error_code = COALESCE(error_code, 'effect_outcome_unknown')");
            setState(c, id, OperationState.RECOVERY_REQUIRED);
        } else if (op.state() == OperationState.CANCELLING) {
            update(c, id, "owner_token = NULL");
            finish(c, id, OperationState.CANCELLED);
        } else {
            update(c, id, "state = ?, owner_token = NULL, external_effect_started = 0, progress_percent = 0, "
                    + "progress_message = NULL", OperationState.QUEUED.wireName());
            releaseLocks(c, id);
        }
    }

    private static void requireOwner(Connection c, Operation op, String ownerToken) throws SQLException {
        String actual;
        try (PreparedStatement ps = c.prepareStatement("SELECT owner_token FROM operations WHERE id = ?")) {
            ps.setString(1, op.id());
            try (ResultSet rs = ps.executeQuery()) {
                actual = rs.next() ? rs.getString(1) : null;
            }
        }
        if (!Objects.equals(actual, ownerToken)) {
            throw new OperationException(OperationException.NOT_OWNER, "Operation is not owned by this caller");
        }
    }

    private Optional<OperationInput> loadInput(Connection c, String id) throws SQLException {
        try (PreparedStatement ps = c.prepareStatement(
                "SELECT input_status, input_json, payload_hash FROM operations WHERE id = ?")) {
            ps.setString(1, id);
            try (ResultSet rs = ps.executeQuery()) {
                if (!rs.next()) {
                    return Optional.empty();
                }
                OperationInput.InputStatus status = OperationInput.InputStatus.fromWire(rs.getString(1));
                String json = rs.getString(2);
                String hash = rs.getString(3);
                if (status == OperationInput.InputStatus.AVAILABLE
                        && (json == null || !StateValues.sha256Hex(json).equals(hash))) {
                    // Stored input no longer matches what was authorized/confirmed: never execute it.
                    return Optional.of(new OperationInput(id, OperationInput.InputStatus.UNAVAILABLE_LEGACY, null, hash));
                }
                return Optional.of(new OperationInput(id, status,
                        status == OperationInput.InputStatus.AVAILABLE ? json : null, hash));
            }
        }
    }

    private void finish(Connection c, String id, OperationState terminal) throws SQLException {
        update(c, id, "state = ?, finished_at = ?, owner_token = NULL", terminal.wireName(), db.clock().millis());
        releaseLocks(c, id);
    }

    private static void releaseLocks(Connection c, String id) throws SQLException {
        try (PreparedStatement ps = c.prepareStatement("DELETE FROM resource_locks WHERE operation_id = ?")) {
            ps.setString(1, id);
            ps.executeUpdate();
        }
    }

    private void update(Connection c, String id, String setClause, Object... params) throws SQLException {
        try (PreparedStatement ps = c.prepareStatement(
                "UPDATE operations SET " + setClause + ", updated_at = ? WHERE id = ?")) {
            int i = 1;
            for (Object p : params) {
                ps.setObject(i++, p);
            }
            ps.setLong(i++, db.clock().millis());
            ps.setString(i, id);
            if (ps.executeUpdate() != 1) {
                throw notFound(id);
            }
        }
    }

    private Optional<Operation> findByIdempotencyKey(Connection c, String principalId, String key) throws SQLException {
        try (PreparedStatement ps = c.prepareStatement(
                "SELECT id FROM operations WHERE server_id = ? AND actor_principal_id = ? AND idempotency_key = ?")) {
            ps.setString(1, db.serverId());
            ps.setString(2, principalId);
            ps.setString(3, key);
            try (ResultSet rs = ps.executeQuery()) {
                return rs.next() ? load(c, rs.getString(1)) : Optional.empty();
            }
        }
    }

    private static List<String> resources(Connection c, String id) throws SQLException {
        List<String> out = new ArrayList<>();
        try (PreparedStatement ps = c.prepareStatement(
                "SELECT resource FROM operation_resources WHERE operation_id = ? ORDER BY resource")) {
            ps.setString(1, id);
            try (ResultSet rs = ps.executeQuery()) {
                while (rs.next()) {
                    out.add(rs.getString(1));
                }
            }
        }
        return List.copyOf(out);
    }

    private static Operation map(ResultSet rs, List<String> resources) throws SQLException {
        long finished = rs.getLong(23);
        Instant finishedAt = rs.wasNull() ? null : Instant.ofEpochMilli(finished);
        return new Operation(rs.getString(1), rs.getLong(2), rs.getString(3), rs.getString(4), rs.getString(5),
                rs.getString(6), rs.getString(7), rs.getString(8), rs.getString(9), rs.getString(10),
                OperationState.fromWire(rs.getString(11)), rs.getInt(12) == 1, rs.getInt(13), rs.getString(14),
                rs.getInt(15) == 1, rs.getInt(16) == 1, rs.getInt(17) == 1, rs.getString(18), rs.getString(19),
                rs.getString(20), Instant.ofEpochMilli(rs.getLong(21)), Instant.ofEpochMilli(rs.getLong(22)),
                finishedAt, resources);
    }

    /** Exact replay check: every safety/lock-relevant field must match the original request. */
    private static boolean sameBinding(Operation op, OperationRequest r, String payloadHash) {
        return op.payloadHash().equals(payloadHash)
                && op.type().equals(r.type())
                && op.target().equals(r.target())
                && op.deviceId().equals(r.actor().deviceId())
                && op.source().equals(r.actor().source())
                && op.lockResources().equals(r.lockResources()) // both sorted, de-duplicated
                && op.externalEffectIdempotent() == r.externalEffectIdempotent()
                && op.requiresConfirmation() == r.requiresConfirmation();
    }

    private static String stateJson(String id, OperationState state, int progress) {
        Map<String, Object> m = new LinkedHashMap<>();
        m.put("operationId", id);
        m.put("state", state.wireName());
        m.put("progress", progress);
        return GSON.toJson(m);
    }

    private static String boundedOrReject(String value, int max, String name) {
        if (value != null && value.length() > max) {
            throw new OperationException(OperationException.INVALID_REQUEST, name + " too large");
        }
        return value;
    }

    static OperationException notFound(String id) {
        return new OperationException(OperationException.NOT_FOUND, "Operation not found: " + id);
    }

    private static OperationException invalid(Operation op, String action) {
        return new OperationException(OperationException.INVALID_TRANSITION,
                "Cannot " + action + " from state " + op.state().wireName());
    }
}
