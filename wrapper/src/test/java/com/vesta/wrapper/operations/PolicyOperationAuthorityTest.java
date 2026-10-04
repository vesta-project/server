package com.vesta.wrapper.operations;

import com.google.gson.JsonObject;
import com.google.gson.JsonParser;
import com.vesta.wrapper.audit.AuditActor;
import com.vesta.wrapper.audit.AuditLog;
import com.vesta.wrapper.authorization.Capability;
import com.vesta.wrapper.files.ServerPathPolicy;
import com.vesta.wrapper.identity.IdentityRecords.RemoteActor;
import com.vesta.wrapper.identity.IdentityService;
import com.vesta.wrapper.identity.LocalAuthority;
import com.vesta.wrapper.state.EventLog;
import com.vesta.wrapper.state.StateDatabase;
import com.vesta.wrapper.state.StateDirectory;
import com.vesta.wrapper.state.StateException;
import com.vesta.wrapper.state.StateValues;
import org.junit.jupiter.api.AfterEach;
import org.junit.jupiter.api.BeforeEach;
import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.io.TempDir;
import org.junit.jupiter.params.ParameterizedTest;
import org.junit.jupiter.params.provider.ValueSource;

import java.nio.file.Path;
import java.time.Clock;
import java.time.Duration;
import java.time.Instant;
import java.time.ZoneId;
import java.time.ZoneOffset;
import java.util.ArrayList;
import java.util.List;
import java.util.Optional;
import java.util.Set;
import java.util.UUID;
import java.util.concurrent.CountDownLatch;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.atomic.AtomicInteger;
import java.util.function.BooleanSupplier;

import static org.junit.jupiter.api.Assertions.*;

class PolicyOperationAuthorityTest {
    @TempDir Path root;
    private StateDirectory directory;
    private StateDatabase db;
    private AuditLog audit;
    private OperationService ops;
    private IdentityService identities;
    private final MutableClock clock = new MutableClock();
    private final List<OperationRunner> runners = new ArrayList<>();
    private RemoteActor owner;
    private static final HandlerContract SAFE = new HandlerContract(true, false, true);
    private static final String INPUT = "{\"path\":\"config/options.txt\",\"value\":\"requested\"}";
    private record Proof(LocalAuthority.Attestation attestation) implements LocalAuthority.Proof { }
    private record Input(String path, String value) { }
    @FunctionalInterface private interface Body { String execute(Input input, OperationContext context) throws Exception; }

    @BeforeEach void open() {
        directory = new StateDirectory(root.resolve("state"));
        db = StateDatabase.open(directory, clock);
        audit = new AuditLog(db);
        ops = new OperationService(db, audit, new EventLog(db));
        identities = new IdentityService(db, audit, (proof, expected) -> proof instanceof Proof p
                ? Optional.of(p.attestation()) : Optional.empty());
        var code = identities.issueOwnerBootstrapCode(proof(LocalAuthority.Purpose.OWNER_BOOTSTRAP), null);
        var paired = identities.redeemOwnerCode(code.code(), "Fixture owner", "Fixture device", StateValues.sha256Hex("owner key"));
        owner = new RemoteActor(paired.principalId(), paired.deviceId());
    }

    @AfterEach void close() throws Exception {
        try {
            for (OperationRunner runner : runners) runner.close();
            for (OperationRunner runner : runners) await(runner::fullyStopped);
        } finally { db.close(); }
    }

    private Proof proof(LocalAuthority.Purpose purpose) {
        return new Proof(new LocalAuthority.Attestation(UUID.randomUUID().toString(), purpose,
                "fixture-os-user", clock.instant(), clock.instant().plusSeconds(60)));
    }

    private RemoteActor subject(String role) {
        String principal = UUID.randomUUID().toString(), device = UUID.randomUUID().toString();
        db.write(c -> {
            try (var ps = c.prepareStatement("INSERT INTO principals VALUES (?,?,?,'active',?,?)")) {
                ps.setString(1, principal); ps.setString(2, db.serverId()); ps.setString(3, "Fixture");
                ps.setLong(4, clock.millis()); ps.setLong(5, clock.millis()); ps.executeUpdate();
            }
            try (var ps = c.prepareStatement("INSERT INTO devices (id,server_id,principal_id,label,public_key_sha256,created_at) VALUES (?,?,?,?,?,?)")) {
                ps.setString(1, device); ps.setString(2, db.serverId()); ps.setString(3, principal);
                ps.setString(4, "Fixture"); ps.setString(5, StateValues.sha256Hex(device)); ps.setLong(6, clock.millis()); ps.executeUpdate();
            }
            return null;
        });
        identities.assignRoleLocally(proof(LocalAuthority.Purpose.LOCAL_ROLE_GRANT), principal, role, null);
        return new RemoteActor(principal, device);
    }

    private void override(RemoteActor actor, Capability capability, String scope, boolean deny) {
        db.write(c -> {
            try (var ps = c.prepareStatement("INSERT INTO principal_overrides (id,principal_id,capability,scope,effect,granted_by,created_at) VALUES (?,?,?,?,?,'fixture',?)")) {
                ps.setString(1, UUID.randomUUID().toString()); ps.setString(2, actor.principalId()); ps.setString(3, capability.id());
                ps.setString(4, scope); ps.setString(5, deny ? "deny" : "allow"); ps.setLong(6, clock.millis()); ps.executeUpdate();
            }
            return null;
        });
    }

    private OperationBinding<Input> binding(String type, HandlerContract contract, List<Capability> caps, Body body) {
        OperationHandler<Input> handler = new OperationHandler<>() {
            public String type() { return type; }
            public HandlerContract contract() { return contract; }
            public Input parseInput(String json) {
                var parsed = JsonParser.parseString(json).getAsJsonObject();
                return new Input(ServerPathPolicy.normalize(parsed.get("path").getAsString(), false),
                        parsed.get("value").getAsString());
            }
            public Set<String> requiredResources(Input input) { return Set.of("files:" + input.path()); }
            public String execute(Input input, OperationContext context) throws Exception { return body.execute(input, context); }
        };
        return OperationBinding.of(handler, new OperationBinding.Derivation<>() {
            public String canonicalInput(Input input) {
                JsonObject object = new JsonObject(); object.addProperty("path", input.path()); object.addProperty("value", input.value());
                return object.toString();
            }
            public String target(Input input) { return input.path(); }
            public List<OperationBinding.Requirement> requirements(Input input) {
                return caps.stream().map(cap -> new OperationBinding.Requirement(cap, cap.scoped() ? input.path() : "")).toList();
            }
        });
    }

    private OperationBindings bindings() {
        return OperationBindings.of(List.of(binding("fixture.write", SAFE, List.of(Capability.FILES_OVERWRITE),
                (input, context) -> { context.beginExternalEffect(); return "{}"; })));
    }
    private OperationSubmissionService submissions(OperationBindings bindings) {
        return new OperationSubmissionService(ops, identities.authorization(), bindings);
    }
    private PolicyOperationAuthority authority(OperationBindings bindings) {
        return new PolicyOperationAuthority(ops, identities.authorization(), bindings);
    }
    private int count(String table) {
        return db.read(c -> { try (var row = c.createStatement().executeQuery("SELECT count(*) FROM " + table)) { row.next(); return row.getInt(1); } });
    }
    private long rejections() { return audit.page(0, 100).stream().filter(e -> e.action().equals("operation.submit") && e.outcome().equals("rejected")).count(); }
    private void auditFailure(boolean fail) {
        db.write(c -> { c.createStatement().execute(fail
                ? "CREATE TRIGGER audit_failure BEFORE INSERT ON audit_events BEGIN SELECT RAISE(ABORT,'injected'); END"
                : "DROP TRIGGER audit_failure"); return null; });
    }
    private static void await(BooleanSupplier condition) throws Exception {
        long deadline = System.nanoTime() + TimeUnit.SECONDS.toNanos(8);
        while (!condition.getAsBoolean() && System.nanoTime() < deadline) Thread.sleep(10);
        assertTrue(condition.getAsBoolean(), "condition did not complete within eight seconds");
    }

    @Test void submissionDerivesCanonicalTargetLocksAndContractIgnoringRequestSideFields() {
        var bindings = OperationBindings.of(List.of(binding("fixture.write", new HandlerContract(false, true, false),
                List.of(Capability.FILES_OVERWRITE), (input, context) -> "{}")));
        var result = submissions(bindings).submit(owner, "https", "fixture.write",
                "{\"path\":\"config\\\\options.txt\",\"value\":\"requested\",\"target\":\"../escape\",\"lockResources\":[],\"requiresConfirmation\":false,\"externalEffectIdempotent\":true}", "key");
        Operation op = result.operation();
        assertEquals("config/options.txt", op.target());
        assertEquals(List.of("files:config/options.txt"), op.lockResources());
        assertFalse(op.externalEffectIdempotent()); assertTrue(op.requiresConfirmation());
        assertEquals(OperationState.AWAITING_CONFIRMATION, op.state());
        assertEquals(INPUT, ops.input(op.id()).orElseThrow().canonicalJson());
        for (var stage : OperationAuthority.Stage.values()) assertTrue(authority(bindings).recheck(op, stage).allowed());
        assertTrue(audit.verifyChain().valid());
    }

    @Test void idempotentRetrievalStillRequiresCurrentAuthorityAndConflictsAreAudited() {
        var bindings = bindings(); var service = submissions(bindings);
        Operation first = service.submit(owner, "https", "fixture.write", INPUT, "key").operation();
        var same = service.submit(owner, "https", "fixture.write", INPUT, "key");
        assertEquals(first.id(), same.operation().id()); assertFalse(same.created());
        assertEquals(OperationException.IDEMPOTENCY_CONFLICT, assertThrows(OperationException.class,
                () -> service.submit(owner, "https", "fixture.write", INPUT.replace("requested", "different"), "key")).getCode());
        assertEquals(1, rejections());
        identities.revokeDevice(owner, owner.deviceId());
        assertEquals(OperationException.AUTHORIZATION_DENIED, assertThrows(OperationException.class,
                () -> service.submit(owner, "https", "fixture.write", INPUT, "key")).getCode());
        assertEquals(1, count("operations")); assertEquals(2, rejections());
    }

    @ParameterizedTest @ValueSource(strings = {"unknown-type", "invalid-json", "traversal", "no-requirements"})
    void preDerivationRefusalsAuditWithoutCreatingOperations(String mutation) {
        var bindings = mutation.equals("no-requirements")
                ? OperationBindings.of(List.of(binding("fixture.write", SAFE, List.of(), (i, c) -> "{}"))) : bindings();
        String type = mutation.equals("unknown-type") ? "fixture.unknown" : "fixture.write";
        String raw = mutation.equals("invalid-json") ? "not json" : mutation.equals("traversal") ? INPUT.replace("config/options.txt", "../escape") : INPUT;
        assertThrows(OperationException.class, () -> submissions(bindings).submit(owner, "https", type, raw, null));
        assertEquals(0, count("operations")); assertEquals(1, rejections()); assertTrue(audit.verifyChain().valid());
    }

    @Test void sourceLoopbackLabelUnknownIdentityAndSystemActorNeverGrantAuthority() {
        var bindings = bindings(); var service = submissions(bindings);
        for (String source : List.of("loopback", "local", "op", "owner", "https")) {
            assertEquals(OperationException.AUTHORIZATION_DENIED, assertThrows(OperationException.class,
                    () -> service.submit(new RemoteActor("unknown", "unknown"), source, "fixture.write", INPUT, null)).getCode());
        }
        assertEquals(0, count("operations")); assertEquals(5, rejections());
        Operation system = ops.submit(new OperationRequest(AuditActor.system("local"), "fixture.write", "config/options.txt",
                INPUT, null, List.of("files:config/options.txt"), true, false)).operation();
        assertFalse(authority(bindings).recheck(system, OperationAuthority.Stage.CLAIM).allowed());
        assertFalse(authority(bindings).recheck(null, OperationAuthority.Stage.CLAIM).allowed());
        assertFalse(authority(bindings).recheck(system, null).allowed());
    }

    @Test void exactDeniesAndAllRequiredCapabilitiesAreEvaluatedIncludingOwner() {
        var bindings = bindings();
        override(owner, Capability.FILES_OVERWRITE, "CONFIG/**", true);
        assertEquals(OperationException.AUTHORIZATION_DENIED, assertThrows(OperationException.class,
                () -> submissions(bindings).submit(owner, "https", "fixture.write", INPUT, null)).getCode());
        RemoteActor viewer = subject("viewer"); override(viewer, Capability.FILES_OVERWRITE, "config/**", false);
        var multiple = OperationBindings.of(List.of(binding("fixture.write", SAFE,
                List.of(Capability.FILES_OVERWRITE, Capability.LIFECYCLE_FORCE_KILL), (i, c) -> "{}")));
        assertThrows(OperationException.class, () -> submissions(multiple).submit(viewer, "https", "fixture.write", INPUT, null));
        assertEquals(0, count("operations"));
    }

    @Test void scopedAllowNeverGrantsSiblingPathsAndImportedReservedGrantsStayDenied() {
        RemoteActor viewer = subject("viewer"); override(viewer, Capability.FILES_OVERWRITE, "config/**", false);
        var bindings = bindings();
        assertNotNull(submissions(bindings).submit(viewer, "https", "fixture.write", INPUT, null));
        assertThrows(OperationException.class,
                () -> submissions(bindings).submit(viewer, "https", "fixture.write", INPUT.replace("config/options.txt", "config-other/options.txt"), null));
        override(viewer, Capability.BACKUPS_RESTORE, "*", false);
        var reserved = OperationBindings.of(List.of(binding("fixture.restore", SAFE, List.of(Capability.BACKUPS_RESTORE), (i, c) -> "{}")));
        assertThrows(OperationException.class, () -> submissions(reserved).submit(viewer, "https", "fixture.restore", INPUT, null));
        var local = OperationBindings.of(List.of(binding("fixture.local", SAFE, List.of(Capability.OWNER_RECOVERY), (i, c) -> "{}")));
        assertThrows(OperationException.class, () -> submissions(local).submit(owner, "loopback", "fixture.local", INPUT, null));
    }

    @ParameterizedTest @ValueSource(strings = {"revoked-device", "disabled-principal", "expired-role", "mismatched-device"})
    void freshPolicyAtEveryStageDeniesChangedIdentity(String mutation) {
        var bindings = bindings(); Operation op = submissions(bindings).submit(owner, "https", "fixture.write", INPUT, null).operation();
        if (mutation.equals("revoked-device")) identities.revokeDevice(owner, owner.deviceId());
        else if (mutation.equals("disabled-principal")) db.write(c -> { c.createStatement().execute("UPDATE principals SET status='disabled'"); return null; });
        else if (mutation.equals("expired-role")) {
            db.write(c -> { c.createStatement().execute("UPDATE role_assignments SET expires_at=" + (clock.millis() + 1000)); return null; });
            clock.now = clock.now.plusSeconds(2);
        } else {
            RemoteActor other = subject("viewer");
            db.write(c -> { try (var ps = c.prepareStatement("UPDATE devices SET principal_id=? WHERE id=?")) {
                ps.setString(1, other.principalId()); ps.setString(2, owner.deviceId()); ps.executeUpdate(); } return null; });
        }
        for (var stage : OperationAuthority.Stage.values()) assertFalse(authority(bindings).recheck(op, stage).allowed(), stage.toString());
    }

    @ParameterizedTest @ValueSource(strings = {"target", "locks", "confirmation", "idempotency", "hash", "purged", "unbound", "wrong-server"})
    void corruptOrMismatchedStoredBindingsNeverRun(String mutation) {
        var bindings = bindings(); Operation original = submissions(bindings).submit(owner, "https", "fixture.write", INPUT, null).operation();
        db.write(c -> {
            String change = switch (mutation) {
                case "target" -> "target='config/other.txt'";
                case "confirmation" -> "requires_confirmation=1";
                case "idempotency" -> "external_effect_idempotent=0";
                case "hash" -> "input_json='{}'";
                case "purged" -> "input_status='purged',input_json=NULL";
                case "unbound" -> "type='fixture.unknown'";
                case "wrong-server" -> "server_id='another-server'";
                default -> null;
            };
            if (change != null) {
                if (mutation.equals("wrong-server")) return null;
                c.createStatement().execute("UPDATE operations SET " + change);
            } else c.createStatement().execute("UPDATE operation_resources SET resource='files:config/other.txt'");
            return null;
        });
        Operation op = ops.get(original.id()).orElseThrow();
        if (mutation.equals("wrong-server")) op = copyWithServerAndHash(op, "another-server", op.payloadHash());
        assertFalse(authority(bindings).recheck(op, OperationAuthority.Stage.CLAIM).allowed());
    }

    @Test void detachedSnapshotPayloadHashAndNullStageAreRejected() {
        var bindings = bindings(); Operation op = submissions(bindings).submit(owner, "https", "fixture.write", INPUT, null).operation();
        assertFalse(authority(bindings).recheck(copyWithServerAndHash(op, op.serverId(), "0".repeat(64)), OperationAuthority.Stage.CLAIM).allowed());
        assertFalse(authority(bindings).recheck(op, null).allowed());
    }

    private static Operation copyWithServerAndHash(Operation op, String server, String hash) {
        return new Operation(op.id(), op.seq(), server, op.actorPrincipalId(), op.deviceId(), op.source(), op.type(), op.target(),
                hash, op.idempotencyKey(), op.state(), op.requiresConfirmation(), op.progressPercent(), op.progressMessage(),
                op.cancelRequested(), op.externalEffectStarted(), op.externalEffectIdempotent(), op.resultJson(), op.errorCode(),
                op.errorMessage(), op.createdAt(), op.updatedAt(), op.finishedAt(), op.lockResources());
    }

    @Test void submissionAndRejectionAuditFailuresCommitNeitherOperationNorAudit() {
        var bindings = bindings(); long before = audit.page(0, 100).size();
        auditFailure(true);
        assertThrows(StateException.class, () -> submissions(bindings).submit(owner, "https", "fixture.write", INPUT, "key"));
        assertThrows(StateException.class, () -> submissions(bindings).submit(new RemoteActor("unknown", "unknown"), "https", "fixture.write", INPUT, null));
        assertThrows(StateException.class, () -> submissions(bindings).submit(owner, "https", "fixture.unknown", INPUT, null));
        assertEquals(0, count("operations")); assertEquals(before, audit.page(0, 100).size());
        auditFailure(false);
        assertTrue(submissions(bindings).submit(owner, "https", "fixture.write", INPUT, "key").created());
    }

    @Test void runnerUsesCanonicalPolicyAndRevocationStopsTheNextExternalEffect() throws Exception {
        CountDownLatch executing = new CountDownLatch(1), release = new CountDownLatch(1);
        AtomicInteger effects = new AtomicInteger();
        var bindings = OperationBindings.of(List.of(binding("fixture.write", SAFE, List.of(Capability.FILES_OVERWRITE), (input, context) -> {
            executing.countDown(); assertTrue(release.await(5, TimeUnit.SECONDS));
            context.beginExternalEffect(); effects.incrementAndGet(); return "{}";
        })));
        Operation op = submissions(bindings).submit(owner, "https", "fixture.write", INPUT, null).operation();
        var runner = new OperationRunner(directory, ops, bindings.handlers(), authority(bindings),
                new OperationRunner.Settings(1, Duration.ofMillis(20), Duration.ofMillis(200)));
        runners.add(runner); runner.start();
        try {
            assertTrue(executing.await(5, TimeUnit.SECONDS));
            identities.revokeDevice(owner, owner.deviceId());
        } finally { release.countDown(); }
        await(() -> ops.get(op.id()).orElseThrow().state() == OperationState.FAILED);
        assertEquals(0, effects.get()); assertFalse(ops.get(op.id()).orElseThrow().externalEffectStarted());
        assertEquals(0, count("resource_locks")); assertTrue(audit.verifyChain().valid());
    }

    @Test void nestedDatabaseCallsRejectRatherThanUsingAnotherSnapshotOrDeadlocking() {
        assertEquals("nested_call", assertThrows(StateException.class, () -> db.read(c -> db.read(cc -> 1))).getCode());
        assertEquals("nested_call", assertThrows(StateException.class, () -> db.read(c -> db.write(cc -> 1))).getCode());
        assertEquals("nested_call", assertThrows(StateException.class, () -> db.write(c -> db.read(cc -> 1))).getCode());
        assertEquals("nested_transaction", assertThrows(StateException.class, () -> db.write(c -> db.write(cc -> 1))).getCode());
        int successfulRead = db.read(c -> 1);
        assertEquals(1, successfulRead);
    }

    @Test void bindingsUseExactlyTheirRegisteredHandlersAndRejectMalformedRequirements() {
        var binding = binding("fixture.write", SAFE, List.of(Capability.FILES_OVERWRITE), (i, c) -> "{}");
        var bindings = OperationBindings.of(List.of(binding));
        assertSame(binding.handler(), bindings.handlers().find("fixture.write").orElseThrow());
        assertTrue(bindings.find("unknown").isEmpty());
        assertThrows(IllegalArgumentException.class, () -> OperationBindings.of(List.of(binding, binding)));
        assertThrows(OperationException.class, () -> new OperationBinding.Requirement(Capability.FILES_READ, "config/**"));
        assertThrows(OperationException.class, () -> new OperationBinding.Requirement(Capability.STATUS_VIEW, "config"));
        var oversized = OperationBindings.of(List.of(binding("fixture.write", SAFE,
                java.util.Collections.nCopies(33, Capability.FILES_READ), (i, c) -> "{}")));
        assertThrows(OperationException.class, () -> submissions(oversized).submit(owner, "https", "fixture.write", INPUT, null));
    }

    @ParameterizedTest @ValueSource(strings = {"target", "requirements", "locks"})
    void canonicalRoundTripMustPreserveEverySecurityDerivation(String difference) {
        OperationHandler<Boolean> handler = new OperationHandler<>() {
            public String type() { return "fixture.lossy"; }
            public HandlerContract contract() { return SAFE; }
            public Boolean parseInput(String json) { return JsonParser.parseString(json).getAsJsonObject().has("elevated"); }
            public Set<String> requiredResources(Boolean elevated) {
                return Set.of(difference.equals("locks") && elevated ? "files:other" : "files:config");
            }
            public String execute(Boolean elevated, OperationContext context) { fail("lossy input must never execute"); return "{}"; }
        };
        var binding = OperationBinding.of(handler, new OperationBinding.Derivation<Boolean>() {
            public String canonicalInput(Boolean elevated) { return "{}"; }
            public String target(Boolean elevated) { return difference.equals("target") && elevated ? "other" : "config"; }
            public List<OperationBinding.Requirement> requirements(Boolean elevated) {
                return List.of(new OperationBinding.Requirement(difference.equals("requirements") && elevated
                        ? Capability.LIFECYCLE_FORCE_KILL : Capability.STATUS_VIEW, ""));
            }
        });
        var bindings = OperationBindings.of(List.of(binding));
        OperationException rejection = assertThrows(OperationException.class,
                () -> submissions(bindings).submit(owner, "https", "fixture.lossy", "{\"elevated\":true}", null));
        assertEquals(OperationException.INVALID_REQUEST, rejection.getCode());
        assertTrue(rejection.getMessage().contains("invalid_input"));
        assertEquals(0, count("operations")); assertEquals(1, rejections());
    }

    @Test void runnerExecutesValidCanonicalSubmissionWithCurrentIdentity() throws Exception {
        AtomicInteger effects = new AtomicInteger();
        var bindings = OperationBindings.of(List.of(binding("fixture.write", SAFE, List.of(Capability.FILES_OVERWRITE), (input, context) -> {
            assertEquals(new Input("config/options.txt", "requested"), input);
            context.beginExternalEffect(); effects.incrementAndGet(); return "{\"written\":true}";
        })));
        Operation op = submissions(bindings).submit(owner, "https", "fixture.write", INPUT, null).operation();
        var runner = new OperationRunner(directory, ops, bindings.handlers(), authority(bindings),
                new OperationRunner.Settings(1, Duration.ofMillis(20), Duration.ofMillis(200)));
        runners.add(runner); runner.start();
        await(() -> ops.get(op.id()).orElseThrow().state() == OperationState.SUCCEEDED);
        assertEquals(1, effects.get()); assertTrue(ops.get(op.id()).orElseThrow().externalEffectStarted());
        assertEquals("{\"written\":true}", ops.get(op.id()).orElseThrow().resultJson());
        assertEquals(0, count("resource_locks")); assertTrue(audit.verifyChain().valid());
    }

    private static final class MutableClock extends Clock {
        private volatile Instant now = Instant.parse("2026-10-04T12:00:00Z");
        public ZoneId getZone() { return ZoneOffset.UTC; }
        public Clock withZone(ZoneId zone) { return this; }
        public Instant instant() { return now; }
    }
}
