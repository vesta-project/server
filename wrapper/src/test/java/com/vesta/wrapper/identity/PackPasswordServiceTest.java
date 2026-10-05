package com.vesta.wrapper.identity;

import com.vesta.wrapper.audit.AuditActor;
import com.vesta.wrapper.audit.AuditLog;
import com.vesta.wrapper.authorization.AuthorizationService;
import com.vesta.wrapper.authorization.BuiltinRoles;
import com.vesta.wrapper.authorization.Capability;
import com.vesta.wrapper.identity.IdentityRecords.RemoteActor;
import com.vesta.wrapper.operations.CanonicalOperationFixture;
import com.vesta.wrapper.security.PasswordKdfGate;
import com.vesta.wrapper.state.StateDatabase;
import com.vesta.wrapper.state.StateException;
import org.junit.jupiter.api.AfterEach;
import org.junit.jupiter.api.BeforeEach;
import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.io.TempDir;
import org.junit.jupiter.params.ParameterizedTest;
import org.junit.jupiter.params.provider.ValueSource;

import java.nio.file.Path;
import java.time.Duration;
import java.util.Arrays;
import java.util.UUID;
import java.util.concurrent.CountDownLatch;
import java.util.concurrent.ExecutionException;
import java.util.concurrent.Executors;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.atomic.AtomicInteger;

import static com.vesta.wrapper.identity.PackPasswordService.VerificationStatus.*;
import static org.junit.jupiter.api.Assertions.*;

class PackPasswordServiceTest {
    @TempDir Path root;
    private CanonicalOperationFixture fixture;
    private PackPasswordService service;
    private PasswordKdfGate gate;
    private final PasswordHasher hasher = new PasswordHasher(PasswordHasher.HARD_MIN, PasswordHasher.HARD_MIN);
    private final AtomicInteger hashes = new AtomicInteger();
    private final AtomicInteger verifications = new AtomicInteger();
    private static final String SECRET = "fixture-private pack password 🔐";
    private static final String REPLACEMENT = "different fixture-private passphrase";

    @BeforeEach void open() {
        fixture = new CanonicalOperationFixture(root.resolve("state"));
        gate = new PasswordKdfGate(new PasswordKdfGate.Settings(8, 10_000, 1_000));
        service = service(normalWork(), gate);
    }

    @AfterEach void close() { fixture.close(); }

    private PackPasswordService service(PackPasswordService.PasswordWork work, PasswordKdfGate admission) {
        return new PackPasswordService(fixture.db, fixture.audit, fixture.identities.authorization(), work, admission);
    }

    private PackPasswordService.PasswordWork normalWork() {
        return new PackPasswordService.PasswordWork() {
            public String hash(char[] password) { hashes.incrementAndGet(); return hasher.hash(password); }
            public boolean verify(char[] password, String phc) { verifications.incrementAndGet(); return hasher.verify(password, phc); }
        };
    }

    private void sql(String sql) {
        fixture.db.write(c -> { try (var s = c.createStatement()) { s.execute(sql); } return null; });
    }

    private String storedPhc() {
        return fixture.db.read(c -> { try (var s = c.createStatement(); var r = s.executeQuery("SELECT phc FROM pack_passwords")) {
            return r.next() ? r.getString(1) : null;
        } });
    }

    private long auditCount() { return fixture.count("audit_events"); }
    private PackPasswordService.Metadata create() { return service.setPassword(fixture.owner, 0, SECRET.toCharArray()); }
    private void code(String expected, org.junit.jupiter.api.function.Executable action) {
        StateException failure = assertThrows(StateException.class, action);
        assertEquals(expected, failure.getCode());
        assertFalse(failure.toString().contains(SECRET));
    }

    private static void assertAllSlotsAvailable(PasswordKdfGate gate, int slots) {
        var owned = new java.util.ArrayList<PasswordKdfGate.Permit>();
        try {
            for (int i=0; i<slots; i++) {
                var admission=gate.tryAcquire(PasswordKdfGate.Purpose.VERIFICATION);
                assertEquals(PasswordKdfGate.Status.ADMITTED,admission.status()); owned.add(admission.permit());
            }
            assertEquals(PasswordKdfGate.Status.BUSY,gate.tryAcquire(PasswordKdfGate.Purpose.VERIFICATION).status());
        } finally { owned.forEach(PasswordKdfGate.Permit::close); }
    }

    private static void await(CountDownLatch latch) throws InterruptedException {
        assertTrue(latch.await(10, TimeUnit.SECONDS), "Fixture KDF barrier timed out");
    }

    private PackPasswordService.PasswordWork blockedWork(boolean hash, CountDownLatch entered, CountDownLatch release) {
        var normal = normalWork();
        return new PackPasswordService.PasswordWork() {
            private void block() {
                entered.countDown();
                try { await(release); }
                catch (InterruptedException interrupted) { Thread.currentThread().interrupt(); throw new IllegalStateException("Fixture interrupted"); }
            }
            public String hash(char[] password) { if (hash) block(); return normal.hash(password); }
            public boolean verify(char[] password, String phc) { if (!hash) block(); return normal.verify(password, phc); }
        };
    }

    @Test void createsRotatesRevokesAndReactivatesOneServerScopedSingleton() {
        var absent = service.metadata(fixture.owner); assertEquals(0, absent.revision()); assertFalse(absent.active());
        assertEquals(UNAVAILABLE, service.verify(SECRET.toCharArray()).status()); assertEquals(0, verifications.get());
        char[] caller = SECRET.toCharArray(); char[] original = caller.clone();
        var created = service.setPassword(fixture.owner, 0, caller);
        assertArrayEquals(original, caller); assertEquals(1, created.revision()); assertTrue(created.active());
        assertEquals(fixture.db.serverId(), created.serverId()); assertEquals(fixture.clock.instant(), created.createdAt());
        String firstHash = storedPhc(); assertTrue(firstHash.startsWith("$argon2id$v=19$")); assertFalse(firstHash.contains(SECRET));
        var matched = service.verify(caller); assertTrue(matched.matched()); assertEquals(1, matched.revision());
        assertEquals(fixture.db.serverId(), matched.serverId()); assertArrayEquals(original, caller);
        assertEquals(MISMATCH, service.verify(REPLACEMENT.toCharArray()).status());
        fixture.clock.advance(Duration.ofSeconds(1));
        var rotated = service.setPassword(fixture.owner, 1, REPLACEMENT.toCharArray());
        assertEquals(2, rotated.revision()); assertEquals(created.createdAt(), rotated.createdAt());
        assertNotEquals(firstHash, storedPhc()); assertEquals(MISMATCH, service.verify(caller).status());
        assertTrue(service.verify(REPLACEMENT.toCharArray()).matched());
        var revoked = service.revoke(fixture.owner, 2); assertEquals(3, revoked.revision()); assertFalse(revoked.active());
        assertNotNull(revoked.revokedAt()); assertNull(storedPhc()); assertEquals(UNAVAILABLE, service.verify(caller).status());
        var revived = service.setPassword(fixture.owner, 3, caller); assertEquals(4, revived.revision()); assertTrue(revived.active());
        assertNull(revived.revokedAt()); assertEquals(created.createdAt(), revived.createdAt()); assertEquals(1, fixture.count("pack_passwords"));
        assertTrue(service.verify(caller).matched()); assertTrue(fixture.audit.verifyChain().valid()); Arrays.fill(caller, '\0');
    }

    @Test void persistedVerifierAndAuditSurviveReopenWithoutExposingTheHash() {
        create(); String phc = storedPhc(); String id = fixture.db.serverId(); fixture.db.close();
        try (StateDatabase reopened = StateDatabase.open(fixture.directory, fixture.clock)) {
            var audit = new AuditLog(reopened);
            var reopenedService = new PackPasswordService(reopened, audit, new AuthorizationService(reopened),
                    PackPasswordService.passwordWork(hasher), gate);
            assertEquals(id, reopenedService.metadata(fixture.owner).serverId());
            assertTrue(reopenedService.verify(SECRET.toCharArray()).matched());
            String output = reopenedService.metadata(fixture.owner).toString() + reopenedService.verify(SECRET.toCharArray()) + audit.page(0,100);
            assertFalse(output.contains(SECRET)); assertFalse(output.contains(phc)); assertTrue(audit.verifyChain().valid());
        }
    }

    @Test void freshSaltIsUsedEvenWhenRotatingToTheSamePassword() {
        create(); String first = storedPhc(); service.setPassword(fixture.owner, 1, SECRET.toCharArray());
        assertNotEquals(first, storedPhc()); assertTrue(service.verify(SECRET.toCharArray()).matched());
    }

    @ParameterizedTest @ValueSource(strings = {"null", "unknown", "system", "malformed", "wrong-principal", "wrong-device",
            "revoked", "disabled", "denied", "expired", "assignment-revoked", "maintainer", "pack-user"})
    void everyAdministrativeBoundaryUsesCurrentCanonicalPolicyBeforeHashing(String change) {
        create(); RemoteActor actor = fixture.owner;
        switch (change) {
            case "null" -> actor = null;
            case "unknown" -> actor = new RemoteActor("unknown", "unknown");
            case "system" -> actor = new RemoteActor("system", "local");
            case "malformed" -> actor = new RemoteActor("\n", null);
            case "wrong-principal" -> actor = new RemoteActor(UUID.randomUUID().toString(), fixture.owner.deviceId());
            case "wrong-device" -> actor = new RemoteActor(fixture.owner.principalId(), UUID.randomUUID().toString());
            case "revoked" -> fixture.identities.revokeDevice(fixture.owner, fixture.owner.deviceId());
            case "disabled" -> sql("UPDATE principals SET status='disabled'");
            case "denied" -> fixture.deny(Capability.CREDENTIALS_MANAGE);
            case "expired" -> sql("UPDATE role_assignments SET expires_at=" + fixture.clock.millis());
            case "assignment-revoked" -> sql("UPDATE role_assignments SET revoked_at=" + fixture.clock.millis());
            case "maintainer" -> sql("UPDATE role_assignments SET role_id='" + BuiltinRoles.MAINTAINER + "'");
            case "pack-user" -> sql("UPDATE role_assignments SET role_id='" + BuiltinRoles.PACK_USER + "'");
        }
        RemoteActor caller = actor; String phc = storedPhc(); int before = hashes.get();
        code("forbidden", () -> service.setPassword(caller, 1, REPLACEMENT.toCharArray()));
        code("forbidden", () -> service.revoke(caller, 1));
        code("forbidden", () -> service.metadata(caller));
        assertEquals(before, hashes.get()); assertEquals(phc, storedPhc());
        assertEquals(3, fixture.audit.page(0,100).stream().filter(e -> e.action().startsWith("pack_password.") && e.outcome().equals("denied")).count());
        assertTrue(fixture.audit.verifyChain().valid());
    }

    @Test void administratorWithExactCapabilityCanManageCredentialsButExplicitDenyStillWins() {
        sql("UPDATE role_assignments SET role_id='" + BuiltinRoles.ADMINISTRATOR + "'");
        assertEquals(1, create().revision()); fixture.deny(Capability.CREDENTIALS_MANAGE);
        code("forbidden", () -> service.revoke(fixture.owner, 1)); assertNotNull(storedPhc());
    }

    @Test void anotherServerCannotUseAnActorFromThisServerToMutateItsCredential() {
        try (var other = new CanonicalOperationFixture(root.resolve("other-state"))) {
            var isolated = new PackPasswordService(other.db, other.audit, other.identities.authorization(), normalWork(), gate);
            code("forbidden", () -> isolated.setPassword(fixture.owner, 0, SECRET.toCharArray()));
            assertEquals(0, hashes.get()); assertEquals(0, other.count("pack_passwords"));
        }
    }

    @Test void optimisticRevisionAndOverflowChecksRunBeforeKdfAndNeverResetTombstones() {
        code("invalid_request", () -> service.setPassword(fixture.owner, -1, SECRET.toCharArray()));
        code("credential_conflict", () -> service.setPassword(fixture.owner, 1, SECRET.toCharArray()));
        code("invalid_request", () -> service.revoke(fixture.owner, 0));
        assertEquals(0, hashes.get()); create(); String phc = storedPhc();
        code("credential_conflict", () -> service.setPassword(fixture.owner, 0, SECRET.toCharArray()));
        code("credential_conflict", () -> service.revoke(fixture.owner, 2));
        sql("UPDATE pack_passwords SET revision=" + Long.MAX_VALUE);
        code("credential_revision_exhausted", () -> service.setPassword(fixture.owner, Long.MAX_VALUE, SECRET.toCharArray()));
        code("credential_revision_exhausted", () -> service.revoke(fixture.owner, Long.MAX_VALUE));
        assertEquals(1, hashes.get()); assertEquals(phc, storedPhc());
        sql("UPDATE pack_passwords SET revision=1"); service.revoke(fixture.owner, 1);
        code("credential_conflict", () -> service.setPassword(fixture.owner, 0, SECRET.toCharArray()));
        assertEquals(3, service.revoke(fixture.owner, 2).revision()); assertNull(storedPhc());
    }

    @ParameterizedTest @ValueSource(strings = {"create", "rotate", "revoke", "verification", "refusal"})
    void auditFailureNeverCommitsMutationOrReturnsAMatch(String operation) {
        if (!operation.equals("create")) create();
        String phc = storedPhc(); long before = auditCount();
        fixture.auditFailure(true);
        switch (operation) {
            case "create" -> assertThrows(StateException.class, this::create);
            case "rotate" -> assertThrows(StateException.class, () -> service.setPassword(fixture.owner, 1, REPLACEMENT.toCharArray()));
            case "revoke" -> assertThrows(StateException.class, () -> service.revoke(fixture.owner, 1));
            case "verification" -> assertThrows(StateException.class, () -> service.verify(SECRET.toCharArray()));
            case "refusal" -> code("audit_unavailable", () -> service.setPassword(null, 1, REPLACEMENT.toCharArray()));
        }
        assertEquals(before, auditCount()); assertEquals(phc, storedPhc()); fixture.auditFailure(false);
        assertAllSlotsAvailable(gate,8);
        if (operation.equals("create")) assertEquals(0, service.metadata(fixture.owner).revision());
        else { assertEquals(1, service.metadata(fixture.owner).revision()); assertTrue(service.verify(SECRET.toCharArray()).matched()); }
        assertTrue(fixture.audit.verifyChain().valid());
    }

    @ParameterizedTest @ValueSource(strings = {"empty", "null", "oversized", "malformed-utf16"})
    void invalidCallerPasswordsFailClosedAndPreserveTheCallerBuffer(String kind) {
        char[] password = switch (kind) {
            case "empty" -> new char[0]; case "null" -> null;
            case "oversized" -> new char[PasswordHasher.MAX_PASSWORD_CHARS + 1];
            default -> new char[]{'x','\uD800'};
        };
        char[] before = password == null ? null : password.clone();
        code("invalid_password", () -> service.setPassword(fixture.owner, 0, password)); assertNull(storedPhc());
        create(); assertEquals(INVALID_PASSWORD, service.verify(password).status()); assertArrayEquals(before, password);
        assertAllSlotsAvailable(gate,8);
    }

    @ParameterizedTest @ValueSource(strings = {"plaintext", "wrong-algorithm", "high-memory", "high-iterations", "malformed"})
    void alteredOrOutOfLimitStoredVerifiersRejectWithoutGrantingAnyIdentity(String kind) {
        create(); String phc = storedPhc();
        String corrupted = switch (kind) {
            case "plaintext" -> SECRET; case "wrong-algorithm" -> phc.replace("argon2id", "argon2i");
            case "high-memory" -> phc.replace("m=1024", "m=262144");
            case "high-iterations" -> phc.replace("t=1", "t=16");
            default -> "$argon2id$v=19$m=1,t=1,p=1$a$b";
        };
        fixture.db.write(c -> { try (var ps = c.prepareStatement("UPDATE pack_passwords SET phc=?")) {
            ps.setString(1, corrupted); ps.executeUpdate();
        } return null; });
        var result = service.verify(SECRET.toCharArray()); assertEquals(INVALID_VERIFIER, result.status()); assertFalse(result.matched());
        assertFalse(result.toString().contains(corrupted)); assertFalse(fixture.audit.page(0,100).toString().contains(corrupted));
    }

    @Test void hashAndVerifyKdfFaultsReleaseTheirPermitsAndDoNotLeakEngineMessages() {
        var oneSlot = new PasswordKdfGate(new PasswordKdfGate.Settings(1, 20, 20));
        var normal = normalWork();
        var broken = service(new PackPasswordService.PasswordWork() {
            public String hash(char[] password) { throw new IllegalStateException(SECRET); }
            public boolean verify(char[] password, String phc) { throw new IllegalStateException(SECRET + phc); }
        }, oneSlot);
        StateException error = assertThrows(StateException.class, () -> broken.setPassword(fixture.owner, 0, SECRET.toCharArray()));
        assertEquals("password_kdf_unavailable", error.getCode()); assertNull(error.getCause()); assertFalse(error.toString().contains(SECRET));
        var working = service(normal, oneSlot); assertEquals(1, working.setPassword(fixture.owner,0,SECRET.toCharArray()).revision());
        assertEquals(KDF_UNAVAILABLE, broken.verify(SECRET.toCharArray()).status());
        assertTrue(working.verify(SECRET.toCharArray()).matched()); assertAllSlotsAvailable(oneSlot,1);
        assertFalse(fixture.audit.page(0,100).toString().contains(SECRET));
    }

    @Test void missingRevokedMalformedAndBadPasswordAttemptsAllConsumeVerificationRate() {
        var small = new PasswordKdfGate(new PasswordKdfGate.Settings(1, 4, 20));
        var limited = service(normalWork(), small);
        assertEquals(UNAVAILABLE, limited.verify(null).status());
        limited.setPassword(fixture.owner, 0, SECRET.toCharArray());
        assertEquals(MISMATCH, limited.verify(REPLACEMENT.toCharArray()).status());
        sql("UPDATE pack_passwords SET phc='malformed'"); assertEquals(INVALID_VERIFIER, limited.verify(SECRET.toCharArray()).status());
        limited.revoke(fixture.owner, 1); assertEquals(UNAVAILABLE, limited.verify(SECRET.toCharArray()).status());
        long before = auditCount();
        for (int i = 0; i < 100; i++) assertEquals(RATE_LIMITED, limited.verify(SECRET.toCharArray()).status());
        assertEquals(before, auditCount()); assertEquals(3, limited.setPassword(fixture.owner, 2, SECRET.toCharArray()).revision());
    }

    @Test void mutationRateRejectionDoesNotHashOrGenerateUnboundedAuditWrites() {
        var small = new PasswordKdfGate(new PasswordKdfGate.Settings(1, 100, 1));
        var limited = service(normalWork(), small); limited.setPassword(fixture.owner, 0, SECRET.toCharArray());
        long before = auditCount();
        for (int i = 0; i < 20; i++) code("password_rate_limited", () -> limited.setPassword(fixture.owner, 1, REPLACEMENT.toCharArray()));
        assertEquals(1, hashes.get()); assertEquals(before, auditCount()); assertTrue(limited.verify(SECRET.toCharArray()).matched());
    }

    @Test void noDatabaseTransactionIsHeldDuringEitherKindOfDerivation() {
        var normal = normalWork();
        var reentrant = service(new PackPasswordService.PasswordWork() {
            private void probe() {
                fixture.db.write(c -> { try (var s = c.createStatement()) { s.execute("INSERT OR REPLACE INTO meta VALUES ('kdf-probe','ok')"); } return null; });
                boolean snapshotTransaction = fixture.db.read(c -> !c.getAutoCommit());
                assertTrue(snapshotTransaction);
            }
            public String hash(char[] password) { probe(); return normal.hash(password); }
            public boolean verify(char[] password, String phc) { probe(); return normal.verify(password,phc); }
        }, gate);
        reentrant.setPassword(fixture.owner, 0, SECRET.toCharArray()); assertTrue(reentrant.verify(SECRET.toCharArray()).matched());
    }

    @ParameterizedTest @ValueSource(strings = {"revoked", "disabled", "denied", "expired", "revision-changed"})
    void authorityAndRevisionAreRecheckedAfterHashingBeforeCommit(String change) throws Exception {
        create(); String phc = storedPhc(); CountDownLatch entered = new CountDownLatch(1), release = new CountDownLatch(1);
        var blocked = service(blockedWork(true, entered, release), gate); var executor = Executors.newSingleThreadExecutor();
        try {
            var future = executor.submit(() -> blocked.setPassword(fixture.owner, 1, REPLACEMENT.toCharArray())); await(entered);
            switch (change) {
                case "revoked" -> fixture.identities.revokeDevice(fixture.owner, fixture.owner.deviceId());
                case "disabled" -> sql("UPDATE principals SET status='disabled'");
                case "denied" -> fixture.deny(Capability.CREDENTIALS_MANAGE);
                case "expired" -> sql("UPDATE role_assignments SET expires_at=" + fixture.clock.millis());
                case "revision-changed" -> service.revoke(fixture.owner, 1);
            }
            release.countDown();
            var failure = assertThrows(ExecutionException.class, () -> future.get(10,TimeUnit.SECONDS));
            assertInstanceOf(StateException.class, failure.getCause());
            assertEquals(change.equals("revision-changed") ? "credential_conflict" : "forbidden", ((StateException)failure.getCause()).getCode());
            assertEquals(change.equals("revision-changed") ? null : phc, storedPhc()); assertTrue(fixture.audit.verifyChain().valid());
            assertAllSlotsAvailable(gate,8);
        } finally { release.countDown(); executor.shutdownNow(); assertTrue(executor.awaitTermination(10,TimeUnit.SECONDS)); }
    }

    @Test void concurrentRotationsAtOneExpectedRevisionHaveExactlyOneWinner() throws Exception {
        create(); CountDownLatch entered = new CountDownLatch(2), release = new CountDownLatch(1);
        var blocked = service(blockedWork(true, entered, release), gate); var executor = Executors.newFixedThreadPool(2);
        try {
            var first = executor.submit(() -> blocked.setPassword(fixture.owner,1,REPLACEMENT.toCharArray()));
            var second = executor.submit(() -> blocked.setPassword(fixture.owner,1,SECRET.toCharArray()));
            await(entered); release.countDown(); int winners = 0, conflicts = 0;
            for (var future : java.util.List.of(first,second)) {
                try { assertEquals(2, future.get(10,TimeUnit.SECONDS).revision()); winners++; }
                catch (ExecutionException failure) { assertInstanceOf(StateException.class,failure.getCause()); assertEquals("credential_conflict",((StateException)failure.getCause()).getCode()); conflicts++; }
            }
            assertEquals(1,winners); assertEquals(1,conflicts); assertEquals(2,service.metadata(fixture.owner).revision());
            assertEquals(1, fixture.audit.page(0,100).stream().filter(e -> e.action().equals("pack_password.set") && e.outcome().equals("denied")).count());
            assertTrue(fixture.audit.verifyChain().valid());
        } finally { release.countDown(); executor.shutdownNow(); assertTrue(executor.awaitTermination(10,TimeUnit.SECONDS)); }
    }

    @ParameterizedTest @ValueSource(strings = {"rotate", "revoke", "same-revision-alteration"})
    void aPreviouslyMatchingSnapshotCannotSucceedAfterTheStoredVerifierChanges(String change) throws Exception {
        create(); CountDownLatch entered = new CountDownLatch(1), release = new CountDownLatch(1);
        var blocked = service(blockedWork(false,entered,release),gate); var executor = Executors.newSingleThreadExecutor();
        try {
            var future = executor.submit(() -> blocked.verify(SECRET.toCharArray())); await(entered);
            switch (change) {
                case "rotate" -> service.setPassword(fixture.owner,1,REPLACEMENT.toCharArray());
                case "revoke" -> service.revoke(fixture.owner,1);
                case "same-revision-alteration" -> {
                    String other = hasher.hash(REPLACEMENT.toCharArray());
                    fixture.db.write(c -> { try (var ps=c.prepareStatement("UPDATE pack_passwords SET phc=?")) { ps.setString(1,other); ps.executeUpdate(); } return null; });
                }
            }
            release.countDown(); var result = future.get(10,TimeUnit.SECONDS); assertEquals(CHANGED,result.status()); assertFalse(result.matched());
            assertEquals(1,result.revision()); assertTrue(fixture.audit.verifyChain().valid());
        } finally { release.countDown(); executor.shutdownNow(); assertTrue(executor.awaitTermination(10,TimeUnit.SECONDS)); }
    }

    @Test void busyDerivationsRejectImmediatelyButRevocationStillWorks() throws Exception {
        create(); CountDownLatch entered = new CountDownLatch(1), release = new CountDownLatch(1);
        var oneSlot = new PasswordKdfGate(new PasswordKdfGate.Settings(1,20,20));
        var blocked = service(blockedWork(false,entered,release),oneSlot); var executor=Executors.newSingleThreadExecutor();
        try {
            var future=executor.submit(() -> blocked.verify(SECRET.toCharArray())); await(entered); long before=auditCount();
            assertEquals(BUSY,blocked.verify(SECRET.toCharArray()).status());
            code("password_kdf_busy", () -> blocked.setPassword(fixture.owner,1,REPLACEMENT.toCharArray()));
            assertEquals(before,auditCount()); assertEquals(2,blocked.revoke(fixture.owner,1).revision());
            release.countDown(); assertEquals(CHANGED,future.get(10,TimeUnit.SECONDS).status());
            assertEquals(UNAVAILABLE,blocked.verify(SECRET.toCharArray()).status());
        } finally { release.countDown(); executor.shutdownNow(); assertTrue(executor.awaitTermination(10,TimeUnit.SECONDS)); }
    }

    @ParameterizedTest @ValueSource(strings = {"create", "rotate", "revoke", "verify"})
    void secretBearingSqlFaultsCannotEscapeInExceptionMessagesOrCauseChains(String operation) {
        var latestHash=new java.util.concurrent.atomic.AtomicReference<String>(); var normal=normalWork();
        var instrumented=service(new PackPasswordService.PasswordWork() {
            public String hash(char[] password) { String phc=normal.hash(password); latestHash.set(phc); return phc; }
            public boolean verify(char[] password,String phc) { return normal.verify(password,phc); }
        },gate);
        if (!operation.equals("create")) instrumented.setPassword(fixture.owner,0,SECRET.toCharArray());
        String original=storedPhc(); long before=auditCount();
        switch(operation) {
            case "create" -> sql("CREATE TRIGGER sensitive_sql_fault BEFORE INSERT ON pack_passwords BEGIN SELECT RAISE(ABORT, NEW.phc); END");
            case "rotate", "revoke" -> sql("CREATE TRIGGER sensitive_sql_fault BEFORE UPDATE ON pack_passwords BEGIN SELECT RAISE(ABORT, COALESCE(NEW.phc, OLD.phc)); END");
            case "verify" -> sql("CREATE TRIGGER sensitive_sql_fault BEFORE INSERT ON audit_events BEGIN SELECT RAISE(ABORT, '"+SECRET+" "+original+"'); END");
        }
        StateException failure=assertThrows(StateException.class,() -> {
            switch(operation) {
                case "create" -> instrumented.setPassword(fixture.owner,0,SECRET.toCharArray());
                case "rotate" -> instrumented.setPassword(fixture.owner,1,REPLACEMENT.toCharArray());
                case "revoke" -> instrumented.revoke(fixture.owner,1);
                case "verify" -> instrumented.verify(SECRET.toCharArray());
            }
        });
        assertEquals("state_io",failure.getCode()); assertNull(failure.getCause());
        var output=new java.io.StringWriter(); failure.printStackTrace(new java.io.PrintWriter(output));
        assertFalse(output.toString().contains(SECRET)); assertFalse(output.toString().contains(latestHash.get()));
        assertEquals(original,storedPhc()); assertEquals(before,auditCount()); assertAllSlotsAvailable(gate,8);
    }

    @Test void databaseReadFailureReleasesAdmissionAndRemainsFailClosed() {
        create(); sql("ALTER TABLE pack_passwords RENAME TO hidden_pack_passwords");
        assertThrows(StateException.class,() -> service.verify(SECRET.toCharArray())); assertAllSlotsAvailable(gate,8);
        sql("ALTER TABLE hidden_pack_passwords RENAME TO pack_passwords"); assertTrue(service.verify(SECRET.toCharArray()).matched());
    }

    @Test void shutdownDuringDerivationPreventsCommitAndStillReleasesThePermit() throws Exception {
        create(); CountDownLatch entered=new CountDownLatch(1),release=new CountDownLatch(1);
        var blocked=service(blockedWork(false,entered,release),gate); var executor=Executors.newFixedThreadPool(2);
        try {
            var future=executor.submit(() -> blocked.verify(SECRET.toCharArray())); await(entered);
            executor.submit(fixture.db::close).get(5,TimeUnit.SECONDS);
            release.countDown(); var failure=assertThrows(ExecutionException.class,() -> future.get(10,TimeUnit.SECONDS));
            assertInstanceOf(StateException.class,failure.getCause()); assertEquals("state_closed",((StateException)failure.getCause()).getCode());
            assertAllSlotsAvailable(gate,8);
        } finally { release.countDown(); executor.shutdownNow(); assertTrue(executor.awaitTermination(10,TimeUnit.SECONDS)); }
    }

    @Test void aMatchOrACallerConstructedResultNeverCreatesAPrincipalDeviceOrRoleGrant() {
        create(); assertTrue(service.verify(SECRET.toCharArray()).matched());
        var fabricated=new PackPasswordService.Verification(fixture.db.serverId(),1,MATCH); assertTrue(fabricated.matched());
        assertEquals(1,fixture.count("principals")); assertEquals(1,fixture.count("devices")); assertEquals(1,fixture.count("role_assignments"));
        assertEquals(0,fixture.count("principal_overrides")); assertEquals(0,fixture.count("operations"));
    }

    @Test void successfulAndRejectedResultsNeverPersistPasswordOrPhcOutsideThePrivateColumn() {
        create(); String phc=storedPhc(); code("credential_conflict",this::create);
        var result=service.verify(SECRET.toCharArray()); assertTrue(result.matched());
        String exposed=result+service.metadata(fixture.owner).toString()+fixture.audit.page(0,100);
        assertFalse(exposed.contains(SECRET)); assertFalse(exposed.contains(phc));
        fixture.db.read(c -> { try (var s=c.createStatement()) {
            for (String query : new String[]{"SELECT metadata_json FROM audit_events", "SELECT payload_json FROM events", "SELECT input_json FROM operations"}) {
                try (var rows=s.executeQuery(query)) { while(rows.next()) { String value=rows.getString(1); assertFalse(value!=null && (value.contains(SECRET)||value.contains(phc))); } }
            }
        } return null; });
        assertTrue(fixture.audit.page(0,100).stream().anyMatch(e -> e.action().equals("pack_password.verify") && e.principalId().equals(AuditActor.SYSTEM_PRINCIPAL)));
    }
}
