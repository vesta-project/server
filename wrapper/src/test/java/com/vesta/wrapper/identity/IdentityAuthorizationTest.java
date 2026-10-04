package com.vesta.wrapper.identity;

import com.vesta.wrapper.audit.AuditLog;
import com.vesta.wrapper.authorization.AuthorizationService;
import com.vesta.wrapper.authorization.BuiltinRoles;
import com.vesta.wrapper.authorization.Capability;
import com.vesta.wrapper.authorization.ResourceScope;
import com.vesta.wrapper.identity.IdentityRecords.PairedOwner;
import com.vesta.wrapper.identity.IdentityRecords.RemoteActor;
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
import java.util.List;
import java.util.Map;
import java.util.Optional;
import java.util.Set;
import java.util.UUID;
import java.util.concurrent.Callable;
import java.util.concurrent.Executors;
import java.util.concurrent.TimeUnit;

import static com.vesta.wrapper.authorization.Capability.*;
import static org.junit.jupiter.api.Assertions.*;

class IdentityAuthorizationTest {
    @TempDir Path root;
    private final MutableClock clock = new MutableClock();
    private StateDatabase db;
    private AuditLog audit;
    private IdentityService identities;
    private AuthorizationService authorization;

    private record TestProof(LocalAuthority.Attestation attestation) implements LocalAuthority.Proof { }
    private record Subject(String principalId, String deviceId, String assignmentId) {
        RemoteActor actor() { return new RemoteActor(principalId, deviceId); }
    }

    @BeforeEach void open() {
        db = StateDatabase.open(new StateDirectory(root.resolve("state")), clock);
        audit = new AuditLog(db);
        LocalAuthority testAuthority = (proof, expected) -> proof instanceof TestProof p
                ? Optional.of(p.attestation()) : Optional.empty();
        identities = new IdentityService(db, audit, testAuthority);
        authorization = identities.authorization();
    }

    @AfterEach void close() { db.close(); }

    private TestProof proof(LocalAuthority.Purpose purpose) {
        return new TestProof(new LocalAuthority.Attestation(UUID.randomUUID().toString(), purpose,
                "test-os-user", clock.instant(), clock.instant().plusSeconds(60)));
    }

    private Subject owner() {
        var code = identities.issueOwnerBootstrapCode(proof(LocalAuthority.Purpose.OWNER_BOOTSTRAP), null);
        PairedOwner paired = identities.redeemOwnerCode(code.code(), "Owner", "Owner device", StateValues.sha256Hex("owner-key"));
        return new Subject(paired.principalId(), paired.deviceId(), null);
    }

    /** Trusted test setup, not a pairing endpoint: inserts a principal/device and uses the local-role API. */
    private Subject subject(String roleId) {
        String principal = UUID.randomUUID().toString(), device = UUID.randomUUID().toString();
        db.write(c -> {
            try (var ps = c.prepareStatement("INSERT INTO principals VALUES (?,?,?,'active',?,?)")) {
                ps.setString(1, principal); ps.setString(2, db.serverId()); ps.setString(3, "Fixture");
                ps.setLong(4, clock.millis()); ps.setLong(5, clock.millis()); ps.executeUpdate();
            }
            try (var ps = c.prepareStatement("INSERT INTO devices (id,server_id,principal_id,label,public_key_sha256,created_at) VALUES (?,?,?,?,?,?)")) {
                ps.setString(1, device); ps.setString(2, db.serverId()); ps.setString(3, principal);
                ps.setString(4, "Fixture device"); ps.setString(5, StateValues.sha256Hex(device));
                ps.setLong(6, clock.millis()); ps.executeUpdate();
            }
            return null;
        });
        String assignment = identities.assignRoleLocally(proof(LocalAuthority.Purpose.LOCAL_ROLE_GRANT), principal, roleId, null);
        return new Subject(principal, device, assignment);
    }

    private String storedOverride(Subject subject, Capability capability, String scope, boolean deny, Instant expiry) {
        String id = UUID.randomUUID().toString();
        db.write(c -> {
            try (var ps = c.prepareStatement("INSERT INTO principal_overrides (id,principal_id,capability,scope,effect,granted_by,created_at,expires_at) VALUES (?,?,?,?,?,'fixture',?,?)")) {
                ps.setString(1, id); ps.setString(2, subject.principalId()); ps.setString(3, capability.id());
                ps.setString(4, scope); ps.setString(5, deny ? "deny" : "allow"); ps.setLong(6, clock.millis());
                if (expiry == null) ps.setNull(7, java.sql.Types.INTEGER); else ps.setLong(7, expiry.toEpochMilli());
                ps.executeUpdate();
            }
            return null;
        });
        return id;
    }

    private int count(String table) {
        return db.read(c -> {
            try (var row = c.createStatement().executeQuery("SELECT count(*) FROM " + table)) {
                row.next(); return row.getInt(1);
            }
        });
    }

    private void auditFailure(boolean enabled) {
        db.write(c -> {
            c.createStatement().execute(enabled
                    ? "CREATE TRIGGER audit_failure BEFORE INSERT ON audit_events BEGIN SELECT RAISE(ABORT,'injected'); END"
                    : "DROP TRIGGER audit_failure");
            return null;
        });
    }

    @Test
    void absentOrForeignLocalProofNeverBootstrapsAndRefusalIsAudited() {
        IdentityService unavailable = new IdentityService(db, audit, LocalAuthority.UNAVAILABLE);
        assertEquals("local_authority_required", assertThrows(StateException.class,
                () -> unavailable.issueOwnerBootstrapCode(proof(LocalAuthority.Purpose.OWNER_BOOTSTRAP), null)).getCode());
        assertEquals("local_authority_required", assertThrows(StateException.class,
                () -> identities.issueOwnerBootstrapCode(new LocalAuthority.Proof() { }, null)).getCode());
        assertEquals(0, count("bootstrap_codes"));
        assertEquals(0, count("principals"));
        assertEquals(2, audit.page(0, 100).stream().filter(e -> e.outcome().equals("rejected")).count());
    }

    @ParameterizedTest
    @ValueSource(strings = {"purpose", "expired", "future", "long", "negative-lifetime", "zero-lifetime", "short-nonce", "long-nonce", "actor"})
    void invalidLocalAttestationsCannotConsumeANonceOrGrantOwner(String mutation) {
        var original = proof(LocalAuthority.Purpose.OWNER_BOOTSTRAP).attestation();
        String nonce = switch (mutation) { case "short-nonce" -> "short"; case "long-nonce" -> "x".repeat(257); default -> original.nonce(); };
        var purpose = mutation.equals("purpose") ? LocalAuthority.Purpose.OWNER_RECOVERY : original.purpose();
        Instant issued = switch (mutation) {
            case "future" -> clock.instant().plusSeconds(6);
            case "negative-lifetime" -> clock.instant().plusSeconds(2);
            default -> original.issuedAt();
        };
        Instant expiry = switch (mutation) {
            case "expired", "zero-lifetime" -> clock.instant();
            case "long" -> clock.instant().plusSeconds(121);
            case "negative-lifetime" -> clock.instant().plusSeconds(1);
            default -> original.expiresAt();
        };
        var malformed = new TestProof(new LocalAuthority.Attestation(nonce, purpose,
                mutation.equals("actor") ? "bad\nactor" : original.osActor(), issued, expiry));
        assertEquals("local_authority_required", assertThrows(StateException.class,
                () -> identities.issueOwnerBootstrapCode(malformed, null)).getCode());
        assertEquals(0, count("local_authority_uses"));
        assertEquals(0, count("bootstrap_codes"));
        assertTrue(audit.verifyChain().valid());
    }

    @Test
    void codeAndProofAreSingleUseSecretFreeAndFingerprintsAreCanonical() {
        var proof = proof(LocalAuthority.Purpose.OWNER_BOOTSTRAP);
        var code = identities.issueOwnerBootstrapCode(proof, null);
        assertEquals(clock.instant().plus(IdentityService.DEFAULT_CODE_TTL), code.expiresAt());
        assertFalse(code.toString().contains(code.code()));
        assertFalse(proof.attestation().toString().contains(proof.attestation().nonce()));
        assertEquals("local_authority_required", assertThrows(StateException.class,
                () -> identities.issueOwnerBootstrapCode(proof, null)).getCode());
        String fingerprint = "AB".repeat(32);
        var paired = identities.redeemOwnerCode(code.code(), "Owner", "Device", fingerprint);
        assertEquals(fingerprint.toLowerCase(java.util.Locale.ROOT), identities.devices(paired.principalId()).get(0).publicKeySha256());
        assertEquals("code_invalid", assertThrows(StateException.class,
                () -> identities.redeemOwnerCode(code.code(), "Again", "Again", "cd".repeat(32))).getCode());
        assertEquals("bootstrap_closed", assertThrows(StateException.class,
                () -> identities.issueOwnerBootstrapCode(proof(LocalAuthority.Purpose.OWNER_BOOTSTRAP), null)).getCode());
        assertFalse(audit.page(0, 100).toString().contains(code.code()));
        assertFalse(audit.page(0, 100).toString().contains(proof.attestation().nonce()));
        assertEquals(StateValues.sha256Hex(code.code()), db.read(c -> {
            try (var row = c.createStatement().executeQuery("SELECT code_hash FROM bootstrap_codes")) {
                row.next(); return row.getString(1);
            }
        }));
    }

    @Test
    void codeRotationInvalidatesEarlierCodeAndExpiryRejectsAtExactDeadline() {
        var first = identities.issueOwnerBootstrapCode(proof(LocalAuthority.Purpose.OWNER_BOOTSTRAP), null);
        var second = identities.issueOwnerBootstrapCode(proof(LocalAuthority.Purpose.OWNER_BOOTSTRAP), Duration.ofMinutes(1));
        assertEquals("code_invalid", assertThrows(StateException.class,
                () -> identities.redeemOwnerCode(first.code(), "Owner", "Device", "aa".repeat(32))).getCode());
        clock.advance(Duration.ofMinutes(1));
        assertEquals("code_invalid", assertThrows(StateException.class,
                () -> identities.redeemOwnerCode(second.code(), "Owner", "Device", "aa".repeat(32))).getCode());
        assertEquals(0, count("principals"));
    }

    @Test
    void concurrentRedemptionCreatesOnlyOneOwnerAndOneDevice() throws Exception {
        var code = identities.issueOwnerBootstrapCode(proof(LocalAuthority.Purpose.OWNER_BOOTSTRAP), null);
        var executor = Executors.newFixedThreadPool(4);
        try {
            List<Callable<Boolean>> attempts = java.util.stream.IntStream.range(0, 8).mapToObj(i -> (Callable<Boolean>) () -> {
                try { identities.redeemOwnerCode(code.code(), "Owner", "Device", StateValues.sha256Hex("key-" + i)); return true; }
                catch (StateException e) { assertEquals("code_invalid", e.getCode()); return false; }
            }).toList();
            int successes = 0;
            for (var result : executor.invokeAll(attempts)) if (result.get()) successes++;
            assertEquals(1, successes);
            assertEquals(1, count("principals")); assertEquals(1, count("devices"));
            assertTrue(identities.ownerExists()); assertTrue(audit.verifyChain().valid());
        } finally { executor.shutdownNow(); assertTrue(executor.awaitTermination(5, TimeUnit.SECONDS)); }
    }

    @Test
    void auditFailureRollsBackBootstrapNonceCodeAndRedemptionAndCanBeRetried() {
        var proof = proof(LocalAuthority.Purpose.OWNER_BOOTSTRAP);
        auditFailure(true);
        assertThrows(StateException.class, () -> identities.issueOwnerBootstrapCode(proof, null));
        assertEquals(0, count("bootstrap_codes")); assertEquals(0, count("local_authority_uses"));
        auditFailure(false);
        var code = identities.issueOwnerBootstrapCode(proof, null);
        auditFailure(true);
        assertThrows(StateException.class, () -> identities.redeemOwnerCode(code.code(), "Owner", "Device", "aa".repeat(32)));
        assertEquals(0, count("principals")); assertEquals(0, count("devices"));
        auditFailure(false);
        var paired = identities.redeemOwnerCode(code.code(), "Owner", "Device", "aa".repeat(32));
        assertTrue(authorization.resolve(paired.principalId(), paired.deviceId()).allows(LIFECYCLE_START, null));
        assertTrue(audit.verifyChain().valid());
    }

    @Test
    void failedRefusalAuditReportsAuditUnavailableWithoutMutatingAuthority() {
        owner();
        auditFailure(true);
        assertEquals("audit_unavailable", assertThrows(StateException.class,
                () -> identities.redeemOwnerCode("invalid-code", "Owner", "Device", "ab".repeat(32))).getCode());
        assertEquals(1, count("principals"));
        assertEquals(1, count("devices"));
    }

    @ParameterizedTest
    @ValueSource(strings = {"owner", "administrator", "maintainer", "moderator", "viewer", "pack-user"})
    void builtinRoleDefaultsHaveNoAccidentalLogConsoleOrMutationAuthority(String role) {
        Subject subject = subject(role);
        var policy = authorization.resolve(subject.principalId(), subject.deviceId());
        assertTrue(policy.isAuthenticated());
        assertTrue(policy.allows(STATUS_VIEW, null));
        assertFalse(policy.allows(OWNER_RECOVERY, null)); assertFalse(policy.allows(AGENT_INSTALL, null));
        if (role.equals(BuiltinRoles.OWNER) || role.equals(BuiltinRoles.ADMINISTRATOR)) {
            for (Capability cap : Capability.values()) {
                boolean expected = !cap.localOnly() && (role.equals(BuiltinRoles.OWNER) || !cap.ownerReserved());
                assertEquals(expected, policy.allows(cap, "logs/latest.log"), cap.id());
            }
        } else if (role.equals(BuiltinRoles.MAINTAINER)) {
            Set<Capability> expected = Set.of(STATUS_VIEW, STATUS_DETAILED, METRICS_VIEW, PLAYERS_VIEW,
                    ACTIVITY_VIEW, TASKS_VIEW, DIAGNOSTICS_VIEW, LOGS_READ, CONSOLE_READ, CONSOLE_WRITE,
                    LIFECYCLE_START, LIFECYCLE_STOP, LIFECYCLE_RESTART, LIFECYCLE_FORCE_KILL, MAINTENANCE_TOGGLE,
                    FILES_LIST, FILES_READ, FILES_CREATE, FILES_OVERWRITE, FILES_RENAME, FILES_MKDIR, FILES_DELETE,
                    CONTENT_INSTALL, BACKUPS_VIEW, BACKUPS_CREATE, BACKUPS_VERIFY, SCHEDULES_VIEW, SCHEDULES_EDIT, SCHEDULES_RUN);
            for (Capability cap : Capability.values()) assertEquals(expected.contains(cap), policy.allows(cap, "logs/latest.log"), cap.id());
        } else {
            Set<Capability> expected = switch (role) {
                case "moderator" -> Set.of(STATUS_VIEW, PLAYERS_VIEW, PLAYERS_MODERATE);
                case "viewer" -> Set.of(STATUS_VIEW, STATUS_DETAILED, METRICS_VIEW, PLAYERS_VIEW, ACTIVITY_VIEW, TASKS_VIEW);
                default -> Set.of(STATUS_VIEW, PACK_ACCESS);
            };
            for (Capability cap : Capability.values()) assertEquals(expected.contains(cap), policy.allows(cap, "logs/latest.log"), cap.id());
        }
    }

    @Test
    void unknownDisabledMismatchedAndRevokedIdentitiesNeverResolveAuthority() {
        Subject first = subject(BuiltinRoles.VIEWER), second = subject(BuiltinRoles.MODERATOR);
        assertFalse(authorization.resolve("unknown", "unknown").isAuthenticated());
        assertFalse(authorization.resolve(first.principalId(), second.deviceId()).isAuthenticated());
        db.write(c -> { c.createStatement().executeUpdate("UPDATE principals SET status='disabled' WHERE id='" + first.principalId() + "'"); return null; });
        assertFalse(authorization.resolve(first.principalId(), first.deviceId()).isAuthenticated());
        db.write(c -> { c.createStatement().executeUpdate("UPDATE devices SET revoked_at=" + clock.millis() + " WHERE id='" + second.deviceId() + "'"); return null; });
        assertFalse(authorization.resolve(second.principalId(), second.deviceId()).isAuthenticated());
        assertFalse(authorization.resolve(null, first.deviceId()).isAuthenticated());
    }

    @Test
    void explicitDenyBeatsOwnerAndAnIndividualAllowCannotUndoIt() {
        Subject owner = owner();
        storedOverride(owner, FILES_READ, "config/private/**", true, null);
        storedOverride(owner, FILES_READ, "config/private/**", false, null);
        var policy = authorization.resolve(owner.principalId(), owner.deviceId());
        assertEquals("explicit_deny", policy.check(FILES_READ, "CONFIG/PRIVATE/token.txt").reason());
        assertTrue(policy.allows(FILES_READ, "config/public.txt"));
        assertFalse(policy.holdsEntirely(FILES_READ, ResourceScope.parse("config/**")));
        storedOverride(owner, LIFECYCLE_FORCE_KILL, "*", true, null);
        assertFalse(authorization.resolve(owner.principalId(), owner.deviceId()).allows(LIFECYCLE_FORCE_KILL, null));
        assertTrue(authorization.resolve(owner.principalId(), owner.deviceId()).allows(LIFECYCLE_STOP, null));
    }

    @Test
    void scopesAreCanonicalSegmentBoundedAndCannotWidenDelegatedAuthority() {
        var subtree = ResourceScope.parse("config/**");
        assertTrue(subtree.allows("config")); assertTrue(subtree.allows("config/nested/a.txt"));
        assertFalse(subtree.allows("configuration/a.txt")); assertFalse(subtree.allows("CONFIG/a.txt"));
        assertTrue(subtree.denies("CONFIG/a.txt"));
        assertTrue(subtree.covers(ResourceScope.parse("config/nested/**")));
        assertFalse(ResourceScope.parse("config/a.txt").covers(ResourceScope.parse("config/a.txt/**")));
        assertEquals("logs/x", ResourceScope.parse("logs\\x").text());
        assertEquals("logs/x", ResourceScope.parse("logs/x/").text());
        for (String path : new String[]{"../outside/**", "C:/root/**", "config//x", "config/*/x", "config/CON", "config/a:stream"}) {
            assertThrows(StateException.class, () -> ResourceScope.parse(path), path);
        }
        Subject admin = subject(BuiltinRoles.ADMINISTRATOR), viewer = subject(BuiltinRoles.VIEWER);
        storedOverride(admin, FILES_READ, "saves/**", true, null);
        assertEquals("forbidden", assertThrows(StateException.class,
                () -> identities.setOverride(admin.actor(), viewer.principalId(), FILES_READ, "*", false, null)).getCode());
        identities.setOverride(admin.actor(), viewer.principalId(), FILES_READ, "config/**", false, null);
        var policy = authorization.resolve(viewer.principalId(), viewer.deviceId());
        assertTrue(policy.allows(FILES_READ, "config/options.txt")); assertFalse(policy.allows(FILES_READ, "saves/world.dat"));
    }

    @Test
    void malformedStoredDeniesFailClosedAndReservedTiersCannotBeSmuggledByOverrides() {
        Subject viewer = subject(BuiltinRoles.VIEWER);
        storedOverride(viewer, FILES_READ, "*", false, null);
        storedOverride(viewer, FILES_READ, "../invalid", true, null);
        for (Capability cap : new Capability[]{BACKUPS_RESTORE, RECOVERY_EXPORT, SECURITY_MANAGE, CREDENTIALS_MANAGE, OWNER_RECOVERY}) {
            storedOverride(viewer, cap, "*", false, null);
        }
        var policy = authorization.resolve(viewer.principalId(), viewer.deviceId());
        assertFalse(policy.allows(FILES_READ, "config/options.txt"));
        for (Capability cap : new Capability[]{BACKUPS_RESTORE, RECOVERY_EXPORT, SECURITY_MANAGE, CREDENTIALS_MANAGE, OWNER_RECOVERY}) {
            assertFalse(policy.allows(cap, null), cap.id());
            assertFalse(policy.holdsEntirely(cap, ResourceScope.ALL), cap.id());
        }
        assertTrue(Capability.fromId("*").isEmpty());
        assertTrue(Capability.fromId("lifecycle.manage").isEmpty());
        assertTrue(Capability.fromId("not-a-capability").isEmpty());
    }

    @Test
    void primaryRoleReplacementAndExpiryApplyImmediatelyBeforeAuditSweep() {
        Subject viewer = subject(BuiltinRoles.VIEWER);
        identities.assignRoleLocally(proof(LocalAuthority.Purpose.LOCAL_ROLE_GRANT), viewer.principalId(),
                BuiltinRoles.MODERATOR, clock.instant().plusSeconds(5));
        assertEquals(List.of(BuiltinRoles.MODERATOR), authorization.resolve(viewer.principalId(), viewer.deviceId()).roleIds());
        storedOverride(viewer, FILES_READ, "config/**", false, clock.instant().plusSeconds(5));
        assertTrue(authorization.resolve(viewer.principalId(), viewer.deviceId()).allows(FILES_READ, "config/options.txt"));
        clock.advance(Duration.ofSeconds(5));
        var expired = authorization.resolve(viewer.principalId(), viewer.deviceId());
        assertFalse(expired.allows(PLAYERS_MODERATE, null)); assertFalse(expired.allows(FILES_READ, "config/options.txt"));
        assertEquals(0, expired.rank());
        assertEquals(2, identities.sweepExpired()); assertEquals(0, identities.sweepExpired());
        assertEquals(2, audit.page(0, 100).stream().filter(e -> e.action().equals("authority.expired")).count());
    }

    @Test
    void remoteDelegationIsStrictlyLowerAndCannotCreateAdminOrReservedCustomRoles() {
        Subject owner = owner(), admin = subject(BuiltinRoles.ADMINISTRATOR), maintainer = subject(BuiltinRoles.MAINTAINER),
                moderator = subject(BuiltinRoles.MODERATOR), viewer = subject(BuiltinRoles.VIEWER);
        assertEquals("forbidden", assertThrows(StateException.class,
                () -> identities.assignRole(owner.actor(), viewer.principalId(), BuiltinRoles.ADMINISTRATOR, null)).getCode());
        assertEquals("forbidden", assertThrows(StateException.class,
                () -> identities.assignRole(admin.actor(), viewer.principalId(), BuiltinRoles.OWNER, null)).getCode());
        assertEquals("forbidden", assertThrows(StateException.class,
                () -> identities.assignRole(admin.actor(), owner.principalId(), BuiltinRoles.VIEWER, null)).getCode());
        assertEquals("forbidden", assertThrows(StateException.class,
                () -> identities.assignRole(maintainer.actor(), viewer.principalId(), BuiltinRoles.MODERATOR, null)).getCode());
        for (Capability reserved : new Capability[]{OWNER_RECOVERY, AGENT_INSTALL, BACKUPS_RESTORE, RECOVERY_EXPORT, SECURITY_MANAGE, CREDENTIALS_MANAGE}) {
            assertEquals("forbidden", assertThrows(StateException.class,
                    () -> identities.createCustomRole(owner.actor(), "Smuggle " + reserved.name(), 15, Map.of(reserved, "*"))).getCode());
        }
        identities.assignRole(admin.actor(), viewer.principalId(), BuiltinRoles.MAINTAINER, null);
        assertEquals(BuiltinRoles.MAINTAINER_RANK, authorization.resolve(viewer.principalId(), viewer.deviceId()).rank());
        assertEquals("forbidden", assertThrows(StateException.class,
                () -> identities.createCustomRole(admin.actor(), "Equal rank", BuiltinRoles.ADMINISTRATOR_RANK, Map.of(STATUS_VIEW, "*"))).getCode());
        identities.setOverride(owner.actor(), maintainer.principalId(), ROLES_ASSIGN, "*", false, null);
        assertEquals("forbidden", assertThrows(StateException.class,
                () -> identities.assignRole(maintainer.actor(), moderator.principalId(), BuiltinRoles.MODERATOR, null)).getCode(),
                "default Maintainer lacks players.moderate and cannot grant beyond its own authority");
        String narrowRole = identities.createCustomRole(admin.actor(), "Config reader", 15, Map.of(FILES_READ, "config/**"));
        Subject packUser = subject(BuiltinRoles.PACK_USER);
        identities.assignRole(admin.actor(), packUser.principalId(), narrowRole, null);
        assertTrue(authorization.resolve(packUser.principalId(), packUser.deviceId()).allows(FILES_READ, "config/options.txt"));
        assertFalse(authorization.resolve(packUser.principalId(), packUser.deviceId()).allows(FILES_READ, "logs/latest.log"));
    }

    @Test
    void deviceRevocationHonorsOwnerDenyAndStrictHierarchyWithoutSelfBypass() {
        Subject owner = owner(), admin = subject(BuiltinRoles.ADMINISTRATOR), equal = subject(BuiltinRoles.ADMINISTRATOR), viewer = subject(BuiltinRoles.VIEWER);
        storedOverride(owner, DEVICES_REVOKE, "*", true, null);
        assertEquals("forbidden", assertThrows(StateException.class, () -> identities.revokeDevice(owner.actor(), viewer.deviceId())).getCode());
        assertEquals("forbidden", assertThrows(StateException.class, () -> identities.revokeDevice(admin.actor(), equal.deviceId())).getCode());
        assertEquals("forbidden", assertThrows(StateException.class, () -> identities.revokeDevice(admin.actor(), admin.deviceId())).getCode());
        identities.revokeDevice(admin.actor(), viewer.deviceId());
        assertFalse(authorization.resolve(viewer.principalId(), viewer.deviceId()).isAuthenticated());
        assertFalse(identities.devices(viewer.principalId()).isEmpty());
        assertTrue(identities.devices(viewer.principalId()).get(0).revoked());
    }

    @Test
    void localRecoveryRevokesExistingAdministrativeAccessAndPairsANewOwner() {
        Subject owner = owner(), admin = subject(BuiltinRoles.ADMINISTRATOR), viewer = subject(BuiltinRoles.VIEWER);
        var code = identities.issueOwnerRecoveryCode(proof(LocalAuthority.Purpose.OWNER_RECOVERY), null, true);
        assertFalse(authorization.resolve(owner.principalId(), owner.deviceId()).isAuthenticated());
        assertFalse(authorization.resolve(admin.principalId(), admin.deviceId()).isAuthenticated());
        assertTrue(authorization.resolve(viewer.principalId(), viewer.deviceId()).isAuthenticated());
        assertFalse(identities.ownerExists());
        var recovered = identities.redeemOwnerCode(code.code(), "Recovered owner", "New device", StateValues.sha256Hex("recovered-key"));
        assertTrue(authorization.resolve(recovered.principalId(), recovered.deviceId()).allows(BACKUPS_RESTORE, null));
        assertTrue(identities.ownerExists()); assertTrue(audit.verifyChain().valid());
    }

    @Test
    void auditFailureCannotPartiallyReplaceRolesCreateOverridesOrRevokeDevices() {
        Subject admin = subject(BuiltinRoles.ADMINISTRATOR), viewer = subject(BuiltinRoles.VIEWER);
        auditFailure(true);
        assertThrows(StateException.class, () -> identities.assignRole(admin.actor(), viewer.principalId(), BuiltinRoles.MODERATOR, null));
        assertEquals(List.of(BuiltinRoles.VIEWER), authorization.resolve(viewer.principalId(), viewer.deviceId()).roleIds());
        assertThrows(StateException.class, () -> identities.setOverride(admin.actor(), viewer.principalId(), FILES_READ, "config/**", false, null));
        assertFalse(authorization.resolve(viewer.principalId(), viewer.deviceId()).allows(FILES_READ, "config/a.txt"));
        assertThrows(StateException.class, () -> identities.revokeDevice(admin.actor(), viewer.deviceId()));
        assertFalse(identities.devices(viewer.principalId()).get(0).revoked());
        auditFailure(false); assertTrue(audit.verifyChain().valid());
    }

    @Test
    void builtinRoleRowsAndCapabilityUpdateTargetsAreImmutable() {
        Subject admin = subject(BuiltinRoles.ADMINISTRATOR);
        String custom = identities.createCustomRole(admin.actor(), "Custom read", 15, Map.of(FILES_READ, "config/**"));
        for (String sql : new String[]{
                "UPDATE roles SET rank=1 WHERE id='owner'",
                "DELETE FROM roles WHERE id='owner'",
                "INSERT INTO roles VALUES ('fake-owner','Fake Owner',100,0,'fixture',0)",
                "INSERT INTO roles VALUES ('fake-builtin','Fake Builtin',10,1,'fixture',0)",
                "UPDATE roles SET builtin=1 WHERE id='" + custom + "'",
                "UPDATE role_capabilities SET scope='config/**' WHERE role_id='owner' AND capability='files.read'",
                "UPDATE role_capabilities SET role_id='owner' WHERE role_id='" + custom + "'",
                "DELETE FROM role_capabilities WHERE role_id='owner'",
                "INSERT INTO role_capabilities VALUES ('owner','fake.capability','*')"}) {
            assertThrows(StateException.class, () -> db.write(c -> { c.createStatement().executeUpdate(sql); return null; }), sql);
        }
    }

    @Test
    void reservedCapabilitiesInCustomRoleRowsDoNotBecomeEffectiveRegardlessOfRank() {
        Subject admin = subject(BuiltinRoles.ADMINISTRATOR);
        String custom = identities.createCustomRole(admin.actor(), "Near admin", 79, Map.of(STATUS_VIEW, "*"));
        Subject target = subject(custom), viewer = subject(BuiltinRoles.VIEWER);
        db.write(c -> {
            try (var ps = c.prepareStatement("INSERT INTO role_capabilities VALUES (?,?, '*')")) {
                for (Capability cap : new Capability[]{BACKUPS_RESTORE, RECOVERY_EXPORT, SECURITY_MANAGE, CREDENTIALS_MANAGE, OWNER_RECOVERY, DEVICES_REVOKE}) {
                    ps.setString(1, custom); ps.setString(2, cap.id()); ps.executeUpdate();
                }
            }
            return null;
        });
        var policy = authorization.resolve(target.principalId(), target.deviceId());
        assertEquals(79, policy.rank());
        assertFalse(policy.holdsBuiltinRole(BuiltinRoles.OWNER));
        assertFalse(policy.holdsBuiltinRole(BuiltinRoles.ADMINISTRATOR));
        for (Capability cap : new Capability[]{BACKUPS_RESTORE, RECOVERY_EXPORT, SECURITY_MANAGE, CREDENTIALS_MANAGE, OWNER_RECOVERY}) {
            assertFalse(policy.allows(cap, null), cap.id());
            assertFalse(policy.holdsEntirely(cap, ResourceScope.ALL), cap.id());
        }
        assertTrue(policy.allows(DEVICES_REVOKE, null));
        assertFalse(AuthorizationService.canRevokeDevice(policy, admin.principalId(), BuiltinRoles.ADMINISTRATOR_RANK).allowed());
        assertTrue(AuthorizationService.canRevokeDevice(policy, viewer.principalId(), BuiltinRoles.VIEWER_RANK).allowed());
    }

    @Test
    void malformedStoredAllowDoesNotGrantAndInvalidResourceNeverMatchesAWholePathGrant() {
        Subject viewer = subject(BuiltinRoles.VIEWER);
        storedOverride(viewer, FILES_READ, "../invalid", false, null);
        assertFalse(authorization.resolve(viewer.principalId(), viewer.deviceId()).allows(FILES_READ, "config/options.txt"));
        storedOverride(viewer, FILES_READ, "*", false, null);
        var policy = authorization.resolve(viewer.principalId(), viewer.deviceId());
        for (String resource : new String[]{"../outside", "C:/root", "//host/share", "config/a:stream", "logs/CON", "config/./x"}) {
            assertEquals("invalid_resource", policy.check(FILES_READ, resource).reason(), resource);
        }
    }

    @ParameterizedTest
    @ValueSource(longs = {-1, 0, 59, 3601})
    void invalidBootstrapCodeLifetimeCannotConsumeTheLocalProof(long seconds) {
        var proof = proof(LocalAuthority.Purpose.OWNER_BOOTSTRAP);
        assertEquals("invalid_request", assertThrows(StateException.class,
                () -> identities.issueOwnerBootstrapCode(proof, Duration.ofSeconds(seconds))).getCode());
        assertEquals(0, count("bootstrap_codes")); assertEquals(0, count("local_authority_uses"));
        assertNotNull(identities.issueOwnerBootstrapCode(proof, Duration.ofMinutes(1)));
    }

    @Test
    void recoveryAuditFailurePreservesOldAdministrativeAccessAndTheLocalProofForRetry() {
        Subject owner = owner(), admin = subject(BuiltinRoles.ADMINISTRATOR);
        var proof = proof(LocalAuthority.Purpose.OWNER_RECOVERY);
        int codes = count("bootstrap_codes"), proofs = count("local_authority_uses");
        auditFailure(true);
        assertThrows(StateException.class, () -> identities.issueOwnerRecoveryCode(proof, null, true));
        assertTrue(authorization.resolve(owner.principalId(), owner.deviceId()).allows(BACKUPS_RESTORE, null));
        assertTrue(authorization.resolve(admin.principalId(), admin.deviceId()).allows(SECURITY_MANAGE, null));
        assertEquals(codes, count("bootstrap_codes")); assertEquals(proofs, count("local_authority_uses"));
        auditFailure(false);
        assertNotNull(identities.issueOwnerRecoveryCode(proof, null, true));
        assertFalse(authorization.resolve(owner.principalId(), owner.deviceId()).isAuthenticated());
        assertFalse(authorization.resolve(admin.principalId(), admin.deviceId()).isAuthenticated());
    }

    @Test
    void expiryAuditFailureDoesNotMarkExpiredAuthorityAsAlreadyReported() {
        Subject viewer = subject(BuiltinRoles.VIEWER);
        storedOverride(viewer, FILES_READ, "config/**", false, clock.instant().plusSeconds(1));
        clock.advance(Duration.ofSeconds(1));
        auditFailure(true);
        assertThrows(StateException.class, identities::sweepExpired);
        assertFalse(authorization.resolve(viewer.principalId(), viewer.deviceId()).allows(FILES_READ, "config/options.txt"));
        auditFailure(false);
        assertEquals(1, identities.sweepExpired()); assertEquals(0, identities.sweepExpired());
        assertTrue(audit.verifyChain().valid());
    }

    @ParameterizedTest
    @ValueSource(strings = {"principals", "devices"})
    void wrongServerIdentityRowsFailClosedEvenIfAnExternalImportBypassedForeignKeys(String table) throws Exception {
        Subject viewer = subject(BuiltinRoles.VIEWER);
        String id = table.equals("principals") ? viewer.principalId() : viewer.deviceId();
        // Simulates a damaged/imported row. Ordinary service transactions enforce foreign keys.
        try (var c = java.sql.DriverManager.getConnection("jdbc:sqlite:" + db.directory().databaseFile().toAbsolutePath())) {
            c.createStatement().execute("PRAGMA foreign_keys=OFF");
            try (var ps = c.prepareStatement("UPDATE " + table + " SET server_id=? WHERE id=?")) {
                ps.setString(1, UUID.randomUUID().toString()); ps.setString(2, id); ps.executeUpdate();
            }
        }
        assertFalse(authorization.resolve(viewer.principalId(), viewer.deviceId()).isAuthenticated());
    }

    private static final class MutableClock extends Clock {
        private Instant now = Instant.parse("2026-10-04T12:00:00Z");
        void advance(Duration duration) { now = now.plus(duration); }
        @Override public ZoneId getZone() { return ZoneOffset.UTC; }
        @Override public Clock withZone(ZoneId zone) { return this; }
        @Override public Instant instant() { return now; }
    }
}
