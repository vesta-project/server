package com.vesta.wrapper.identity;

import com.vesta.wrapper.audit.AuditActor;
import com.vesta.wrapper.audit.AuditLog;
import com.vesta.wrapper.audit.AuditRecord;
import com.vesta.wrapper.authorization.AuthorizationService;
import com.vesta.wrapper.authorization.AuthorizationService.RoleGrant;
import com.vesta.wrapper.authorization.AuthorizationService.RoleInfo;
import com.vesta.wrapper.authorization.BuiltinRoles;
import com.vesta.wrapper.authorization.Capability;
import com.vesta.wrapper.authorization.Decision;
import com.vesta.wrapper.authorization.EffectivePolicy;
import com.vesta.wrapper.authorization.ResourceScope;
import com.vesta.wrapper.identity.IdentityRecords.BootstrapCode;
import com.vesta.wrapper.identity.IdentityRecords.Device;
import com.vesta.wrapper.identity.IdentityRecords.PairedOwner;
import com.vesta.wrapper.identity.IdentityRecords.Principal;
import com.vesta.wrapper.identity.IdentityRecords.RemoteActor;
import com.vesta.wrapper.state.StateDatabase;
import com.vesta.wrapper.state.StateException;
import com.vesta.wrapper.state.StateValues;

import java.security.SecureRandom;
import java.sql.Connection;
import java.sql.PreparedStatement;
import java.sql.ResultSet;
import java.sql.SQLException;
import java.time.Duration;
import java.time.Instant;
import java.util.ArrayList;
import java.util.Base64;
import java.util.LinkedHashSet;
import java.util.List;
import java.util.Locale;
import java.util.Map;
import java.util.Objects;
import java.util.Optional;
import java.util.Set;
import java.util.UUID;
import java.util.function.Supplier;
import java.util.regex.Pattern;

import org.slf4j.Logger;
import org.slf4j.LoggerFactory;

/**
 * Canonical server-local principals, devices, owner bootstrap/recovery and role/override delegation.
 *
 * <p>Every mutation and its audit record commit in one write transaction (an audit failure aborts the
 * mutation). Denied and rejected attempts are audited in a separate transaction after rollback.
 * Remote mutations re-resolve the actor's current policy inside the transaction and enforce the strict
 * rank hierarchy of {@link AuthorizationService}. Local operations require a {@link LocalAuthority}
 * proof; with {@link LocalAuthority#UNAVAILABLE} they always fail.
 *
 * <p>Out of scope here: device key possession/mTLS, sessions, rate limiting, vault, invitations, HTTP.
 * Error codes: {@code forbidden}, {@code local_authority_required}, {@code code_invalid},
 * {@code bootstrap_closed}, {@code not_found}, {@code conflict}, {@code invalid_request}.
 */
public final class IdentityService {
    public static final Duration DEFAULT_CODE_TTL = Duration.ofMinutes(10);
    public static final Duration MIN_CODE_TTL = Duration.ofMinutes(1);
    public static final Duration MAX_CODE_TTL = Duration.ofMinutes(60);
    public static final Duration MAX_PROOF_LIFETIME = Duration.ofMinutes(2);
    static final Duration PROOF_CLOCK_SKEW = Duration.ofSeconds(5);

    private static final String SOURCE_LOCAL = "local-authority";
    private static final String SOURCE_REMOTE = "remote";
    private static final String SOURCE_CODE = "bootstrap-code";
    private static final Pattern HEX64 = Pattern.compile("[0-9a-fA-F]{64}");
    private static final int CODE_BYTES = 20;

    private final StateDatabase db;
    private final AuditLog audit;
    private final AuthorizationService authz;
    private final LocalAuthority localAuthority;
    private final SecureRandom random = new SecureRandom();
    private static final Logger log = LoggerFactory.getLogger(IdentityService.class);
    private static final Set<String> LOCAL_REFUSALS = Set.of(
            "local_authority_required", "bootstrap_closed", "not_found", "invalid_request", "conflict");

    public IdentityService(StateDatabase db, AuditLog audit, LocalAuthority localAuthority) {
        this.db = Objects.requireNonNull(db, "db");
        this.audit = Objects.requireNonNull(audit, "audit");
        this.localAuthority = Objects.requireNonNull(localAuthority, "localAuthority");
        this.authz = new AuthorizationService(db);
    }

    public AuthorizationService authorization() {
        return authz;
    }

    // ================================================================== local (OS-proven) operations

    /** Issues the single-use initial owner code. Refused once any active Owner assignment exists. */
    public BootstrapCode issueOwnerBootstrapCode(LocalAuthority.Proof proof, Duration ttl) {
        Duration life = codeTtl(ttl);
        return local("identity.bootstrap.issue", "owner", () -> db.write(c -> {
            LocalAuthority.Attestation att = consumeProof(c, proof, LocalAuthority.Purpose.OWNER_BOOTSTRAP);
            if (activeOwnerExists(c)) {
                throw new StateException("bootstrap_closed", "an owner already exists; use local recovery");
            }
            BootstrapCode code = insertCode(c, LocalAuthority.Purpose.OWNER_BOOTSTRAP, life);
            audit.append(c, new AuditRecord(localActor(), "identity.bootstrap.issue", "owner", "success", null,
                    Map.of("os_actor", att.osActor(), "expires_at", code.expiresAt().toString())));
            return code;
        }));
    }

    /**
     * Local recovery: issues a single-use owner recovery code and optionally revokes all existing
     * administrative access (every active Owner/Administrator assignment and those principals' devices).
     * Outstanding codes are invalidated either way.
     */
    public BootstrapCode issueOwnerRecoveryCode(LocalAuthority.Proof proof, Duration ttl,
                                                boolean revokeAdministrativeAccess) {
        Duration life = codeTtl(ttl);
        return local("identity.recovery.issue", "owner", () -> db.write(c -> {
            LocalAuthority.Attestation att = consumeProof(c, proof, LocalAuthority.Purpose.OWNER_RECOVERY);
            int assignments = 0;
            int devices = 0;
            if (revokeAdministrativeAccess) {
                long now = now();
                List<String> principals = new ArrayList<>();
                try (PreparedStatement ps = c.prepareStatement(
                        "SELECT DISTINCT principal_id FROM role_assignments WHERE role_id IN (?, ?) "
                                + "AND revoked_at IS NULL AND (expires_at IS NULL OR expires_at > ?)")) {
                    ps.setString(1, BuiltinRoles.OWNER);
                    ps.setString(2, BuiltinRoles.ADMINISTRATOR);
                    ps.setLong(3, now);
                    try (ResultSet rs = ps.executeQuery()) {
                        while (rs.next()) {
                            principals.add(rs.getString(1));
                        }
                    }
                }
                try (PreparedStatement ps = c.prepareStatement(
                        "UPDATE role_assignments SET revoked_at = ?, revoked_by = ? WHERE role_id IN (?, ?) "
                                + "AND revoked_at IS NULL")) {
                    ps.setLong(1, now);
                    ps.setString(2, SOURCE_LOCAL);
                    ps.setString(3, BuiltinRoles.OWNER);
                    ps.setString(4, BuiltinRoles.ADMINISTRATOR);
                    assignments = ps.executeUpdate();
                }
                for (String principalId : principals) {
                    try (PreparedStatement ps = c.prepareStatement(
                            "UPDATE devices SET revoked_at = ?, revoked_by = ? WHERE principal_id = ? AND revoked_at IS NULL")) {
                        ps.setLong(1, now);
                        ps.setString(2, SOURCE_LOCAL);
                        ps.setString(3, principalId);
                        devices += ps.executeUpdate();
                    }
                }
            }
            BootstrapCode code = insertCode(c, LocalAuthority.Purpose.OWNER_RECOVERY, life);
            audit.append(c, new AuditRecord(localActor(), "identity.recovery.issue", "owner", "success", null,
                    Map.of("os_actor", att.osActor(),
                            "revoke_admin_access", Boolean.toString(revokeAdministrativeAccess),
                            "revoked_assignments", Integer.toString(assignments),
                            "revoked_devices", Integer.toString(devices),
                            "expires_at", code.expiresAt().toString())));
            return code;
        }));
    }

    /** Local grant of any role (the only path to Owner/Administrator). Replaces the primary role. */
    public String assignRoleLocally(LocalAuthority.Proof proof, String principalId, String roleId, Instant expiresAt) {
        return local("authority.role.assign", "local-role-grant", () -> db.write(c -> {
            LocalAuthority.Attestation att = consumeProof(c, proof, LocalAuthority.Purpose.LOCAL_ROLE_GRANT);
            requireActivePrincipal(c, principalId);
            RoleInfo role = authz.role(c, requireId("roleId", roleId))
                    .orElseThrow(() -> new StateException("not_found", "role not found"));
            String assignmentId = replacePrimaryRole(c, principalId, role.id(), expiry(expiresAt), SOURCE_LOCAL);
            audit.append(c, new AuditRecord(localActor(), "authority.role.assign", "principal:" + principalId,
                    "success", null, metadata("os_actor", att.osActor(), "role", role.id(),
                    "assignment", assignmentId, "expires_at", expiresAt)));
            return assignmentId;
        }));
    }

    // ================================================================== bootstrap code redemption

    /**
     * Redeems an owner bootstrap/recovery code: atomically consumes it and creates a new principal, its
     * first device and an Owner assignment. Possession of the device key must already have been proven
     * by the (future) pairing transport; this method only records the canonical key fingerprint.
     * Rate limiting of redemption attempts belongs to the transport adapter.
     */
    public PairedOwner redeemOwnerCode(String code, String displayName, String deviceLabel, String devicePublicKeySha256) {
        String name = StateValues.requireText("displayName", displayName, 64);
        String label = StateValues.requireText("deviceLabel", deviceLabel, 64);
        String fingerprint = canonicalFingerprint(devicePublicKeySha256);
        String codeHash = (code == null || code.isBlank() || code.length() > 128)
                ? null : StateValues.sha256Hex(code.trim());
        try {
            return db.write(c -> {
                if (codeHash == null) {
                    throw new StateException("code_invalid", "code is invalid or expired");
                }
                long now = now();
                String codeId;
                String purpose;
                try (PreparedStatement ps = c.prepareStatement(
                        "SELECT id, purpose FROM bootstrap_codes WHERE code_hash = ? AND server_id = ? "
                                + "AND consumed_at IS NULL AND invalidated_at IS NULL AND expires_at > ?")) {
                    ps.setString(1, codeHash);
                    ps.setString(2, db.serverId());
                    ps.setLong(3, now);
                    try (ResultSet rs = ps.executeQuery()) {
                        if (!rs.next()) {
                            throw new StateException("code_invalid", "code is invalid or expired");
                        }
                        codeId = rs.getString(1);
                        purpose = rs.getString(2);
                    }
                }
                try (PreparedStatement ps = c.prepareStatement(
                        "UPDATE bootstrap_codes SET consumed_at = ? WHERE id = ? AND consumed_at IS NULL")) {
                    ps.setLong(1, now);
                    ps.setString(2, codeId);
                    if (ps.executeUpdate() != 1) {
                        throw new StateException("code_invalid", "code is invalid or expired");
                    }
                }
                if (LocalAuthority.Purpose.OWNER_BOOTSTRAP.id().equals(purpose) && activeOwnerExists(c)) {
                    throw new StateException("bootstrap_closed", "an owner already exists; use local recovery");
                }
                String principalId = insertPrincipal(c, name);
                String deviceId = insertDevice(c, principalId, label, fingerprint);
                String assignmentId = replacePrimaryRole(c, principalId, BuiltinRoles.OWNER, null,
                        SOURCE_CODE + ":" + codeId);
                audit.append(c, new AuditRecord(new AuditActor(principalId, deviceId, SOURCE_CODE),
                        "identity.owner.paired", "principal:" + principalId, "success", null,
                        Map.of("purpose", purpose, "code_id", codeId, "assignment", assignmentId)));
                return new PairedOwner(principalId, deviceId);
            });
        } catch (StateException e) {
            if ("code_invalid".equals(e.getCode()) || "bootstrap_closed".equals(e.getCode())) {
                auditRefusal(AuditActor.system(SOURCE_CODE), "identity.owner.pair", "owner", "rejected", e);
            }
            throw e;
        }
    }

    // ================================================================== remote delegation

    /** Remote primary-role assignment under the strict hierarchy (never Owner/Administrator). */
    public String assignRole(RemoteActor actor, String principalId, String roleId, Instant expiresAt) {
        String target = "principal:" + principalId;
        return remote(actor, "authority.role.assign", target, (c, policy) -> {
            requireActivePrincipal(c, principalId);
            RoleInfo role = authz.role(c, requireId("roleId", roleId))
                    .orElseThrow(() -> new StateException("not_found", "role not found"));
            enforce(AuthorizationService.canGrantRole(policy, principalId, authz.principalRank(c, principalId), role));
            String assignmentId = replacePrimaryRole(c, principalId, role.id(), expiry(expiresAt), policy.principalId());
            audit.append(c, new AuditRecord(remoteActor(policy), "authority.role.assign", target, "success", null,
                    metadata("role", role.id(), "assignment", assignmentId, "expires_at", expiresAt)));
            return assignmentId;
        });
    }

    /** Remote per-principal allow/deny override; allows must lie within the actor's own authority. */
    public String setOverride(RemoteActor actor, String principalId, Capability capability, String scope,
                              boolean deny, Instant expiresAt) {
        Objects.requireNonNull(capability, "capability");
        ResourceScope parsed = ResourceScope.parse(scope == null ? "*" : scope);
        if (!capability.scoped() && !parsed.isAll()) {
            throw new StateException("invalid_request", "capability is not path-scoped");
        }
        String target = "principal:" + principalId;
        return remote(actor, "authority.override.set", target, (c, policy) -> {
            requirePrincipal(c, principalId);
            enforce(AuthorizationService.canSetOverride(policy, principalId, authz.principalRank(c, principalId),
                    capability, parsed, deny));
            Long exp = expiry(expiresAt);
            String id = UUID.randomUUID().toString();
            try (PreparedStatement ps = c.prepareStatement(
                    "INSERT INTO principal_overrides (id, principal_id, capability, scope, effect, granted_by, "
                            + "created_at, expires_at) VALUES (?,?,?,?,?,?,?,?)")) {
                ps.setString(1, id);
                ps.setString(2, principalId);
                ps.setString(3, capability.id());
                ps.setString(4, parsed.text());
                ps.setString(5, deny ? "deny" : "allow");
                ps.setString(6, policy.principalId());
                ps.setLong(7, now());
                setNullableLong(ps, 8, exp);
                ps.executeUpdate();
            }
            audit.append(c, new AuditRecord(remoteActor(policy), "authority.override.set", target, "success", null,
                    metadata("override", id, "capability", capability.id(), "scope", parsed.text(),
                            "effect", deny ? "deny" : "allow", "expires_at", expiresAt)));
            return id;
        });
    }

    /** Remote revocation of a role assignment held by a strictly lower-ranked principal. */
    public void revokeAssignment(RemoteActor actor, String assignmentId) {
        revokeGrant(actor, "role_assignments", "authority.role.revoke", assignmentId);
    }

    /** Remote revocation of an override held by a strictly lower-ranked principal. */
    public void revokeOverride(RemoteActor actor, String overrideId) {
        revokeGrant(actor, "principal_overrides", "authority.override.revoke", overrideId);
    }

    private void revokeGrant(RemoteActor actor, String table, String action, String grantId) {
        String id = requireId("grantId", grantId);
        remote(actor, action, "grant:" + id, (c, policy) -> {
            String principalId;
            try (PreparedStatement ps = c.prepareStatement(
                    "SELECT principal_id FROM " + table + " WHERE id = ? AND revoked_at IS NULL")) {
                ps.setString(1, id);
                try (ResultSet rs = ps.executeQuery()) {
                    if (!rs.next()) {
                        throw new StateException("not_found", "grant not found");
                    }
                    principalId = rs.getString(1);
                }
            }
            enforce(AuthorizationService.canRevokeAuthority(policy, principalId, authz.principalRank(c, principalId)));
            try (PreparedStatement ps = c.prepareStatement(
                    "UPDATE " + table + " SET revoked_at = ?, revoked_by = ? WHERE id = ? AND revoked_at IS NULL")) {
                ps.setLong(1, now());
                ps.setString(2, policy.principalId());
                ps.setString(3, id);
                ps.executeUpdate();
            }
            audit.append(c, new AuditRecord(remoteActor(policy), action, "principal:" + principalId, "success",
                    null, Map.of("grant", id)));
            return null;
        });
    }

    /** Remote device revocation: requires exact devices.revoke (deny wins); Owner any device, else strictly lower rank. */
    public void revokeDevice(RemoteActor actor, String deviceId) {
        String id = requireId("deviceId", deviceId);
        remote(actor, "identity.device.revoke", "device:" + id, (c, policy) -> {
            String principalId;
            try (PreparedStatement ps = c.prepareStatement(
                    "SELECT principal_id FROM devices WHERE id = ? AND server_id = ? AND revoked_at IS NULL")) {
                ps.setString(1, id);
                ps.setString(2, db.serverId());
                try (ResultSet rs = ps.executeQuery()) {
                    if (!rs.next()) {
                        throw new StateException("not_found", "device not found");
                    }
                    principalId = rs.getString(1);
                }
            }
            enforce(AuthorizationService.canRevokeDevice(policy, principalId, authz.principalRank(c, principalId)));
            try (PreparedStatement ps = c.prepareStatement(
                    "UPDATE devices SET revoked_at = ?, revoked_by = ? WHERE id = ? AND revoked_at IS NULL")) {
                ps.setLong(1, now());
                ps.setString(2, policy.principalId());
                ps.setString(3, id);
                ps.executeUpdate();
            }
            audit.append(c, new AuditRecord(remoteActor(policy), "identity.device.revoke", "device:" + id,
                    "success", null, Map.of("principal", principalId)));
            return null;
        });
    }

    /**
     * Remote custom-role creation from named capabilities: rank strictly below the actor, no local-only
     * or admin-reserved capability, every grant within the actor's own authority. Custom roles are
     * immutable in this checkpoint (no edit API); revoke assignments and create a new role instead.
     */
    public String createCustomRole(RemoteActor actor, String name, int rank, Map<Capability, String> grants) {
        String roleName = StateValues.requireText("name", name, 32);
        if (grants == null || grants.isEmpty()) {
            throw new StateException("invalid_request", "a role needs at least one capability");
        }
        List<RoleGrant> parsed = new ArrayList<>();
        for (Map.Entry<Capability, String> e : grants.entrySet()) {
            Capability capability = Objects.requireNonNull(e.getKey(), "capability");
            ResourceScope scope = ResourceScope.parse(e.getValue() == null ? "*" : e.getValue());
            if (!capability.scoped() && !scope.isAll()) {
                throw new StateException("invalid_request", "capability is not path-scoped: " + capability.id());
            }
            parsed.add(new RoleGrant(capability, scope));
        }
        return remote(actor, "authority.role.create", "role:" + roleName, (c, policy) -> {
            enforce(AuthorizationService.canCreateRole(policy, rank, parsed));
            String id = "custom-" + UUID.randomUUID();
            try (PreparedStatement ps = c.prepareStatement(
                    "INSERT INTO roles (id, name, rank, builtin, created_by, created_at) VALUES (?,?,?,0,?,?)")) {
                ps.setString(1, id);
                ps.setString(2, roleName);
                ps.setInt(3, rank);
                ps.setString(4, policy.principalId());
                ps.setLong(5, now());
                ps.executeUpdate();
            } catch (SQLException e) {
                if (e.getMessage() != null && e.getMessage().contains("UNIQUE")) {
                    throw new StateException("conflict", "role name already exists");
                }
                throw e;
            }
            for (RoleGrant grant : new LinkedHashSet<>(parsed)) {
                try (PreparedStatement ps = c.prepareStatement(
                        "INSERT INTO role_capabilities (role_id, capability, scope) VALUES (?,?,?)")) {
                    ps.setString(1, id);
                    ps.setString(2, grant.capability().id());
                    ps.setString(3, grant.scope().text());
                    ps.executeUpdate();
                }
            }
            audit.append(c, new AuditRecord(remoteActor(policy), "authority.role.create", "role:" + id, "success",
                    null, Map.of("rank", Integer.toString(rank), "capabilities", Integer.toString(parsed.size()))));
            return id;
        });
    }

    // ================================================================== expiry and reads

    /**
     * Records expiry of role assignments and overrides that have passed {@code expires_at}. Evaluation
     * already ignores expired grants immediately; this only produces the audit trail (and is the hook
     * for future notifications). Returns the number of grants recorded.
     */
    public int sweepExpired() {
        return db.write(c -> {
            long now = now();
            int count = 0;
            for (String table : List.of("role_assignments", "principal_overrides")) {
                List<String[]> expired = new ArrayList<>();
                try (PreparedStatement ps = c.prepareStatement(
                        "SELECT id, principal_id FROM " + table + " WHERE expires_at IS NOT NULL AND expires_at <= ? "
                                + "AND revoked_at IS NULL AND expiry_recorded_at IS NULL LIMIT 500")) {
                    ps.setLong(1, now);
                    try (ResultSet rs = ps.executeQuery()) {
                        while (rs.next()) {
                            expired.add(new String[]{rs.getString(1), rs.getString(2)});
                        }
                    }
                }
                for (String[] row : expired) {
                    try (PreparedStatement ps = c.prepareStatement(
                            "UPDATE " + table + " SET expiry_recorded_at = ? WHERE id = ?")) {
                        ps.setLong(1, now);
                        ps.setString(2, row[0]);
                        ps.executeUpdate();
                    }
                    audit.append(c, new AuditRecord(AuditActor.system("authority-expiry"), "authority.expired",
                            "principal:" + row[1], "success", null, Map.of("grant", row[0], "kind", table)));
                    count++;
                }
            }
            return count;
        });
    }

    public boolean ownerExists() {
        return db.read(this::activeOwnerExists);
    }

    public Optional<Principal> principal(String principalId) {
        return db.read(c -> {
            try (PreparedStatement ps = c.prepareStatement(
                    "SELECT id, display_name, status, created_at FROM principals WHERE id = ? AND server_id = ?")) {
                ps.setString(1, principalId);
                ps.setString(2, db.serverId());
                try (ResultSet rs = ps.executeQuery()) {
                    if (!rs.next()) {
                        return Optional.empty();
                    }
                    return Optional.of(new Principal(rs.getString(1), rs.getString(2), rs.getString(3),
                            Instant.ofEpochMilli(rs.getLong(4))));
                }
            }
        });
    }

    public List<Device> devices(String principalId) {
        return db.read(c -> {
            List<Device> out = new ArrayList<>();
            try (PreparedStatement ps = c.prepareStatement(
                    "SELECT id, principal_id, label, public_key_sha256, created_at, revoked_at FROM devices "
                            + "WHERE principal_id = ? AND server_id = ? ORDER BY created_at, id")) {
                ps.setString(1, principalId);
                ps.setString(2, db.serverId());
                try (ResultSet rs = ps.executeQuery()) {
                    while (rs.next()) {
                        long revoked = rs.getLong(6);
                        Instant revokedAt = rs.wasNull() ? null : Instant.ofEpochMilli(revoked);
                        out.add(new Device(rs.getString(1), rs.getString(2), rs.getString(3), rs.getString(4),
                                Instant.ofEpochMilli(rs.getLong(5)), revokedAt));
                    }
                }
            }
            return List.copyOf(out);
        });
    }

    /** Validates and lower-cases a SHA-256 device public-key fingerprint (64 hex chars). */
    public static String canonicalFingerprint(String fingerprint) {
        if (fingerprint == null || !HEX64.matcher(fingerprint).matches()) {
            throw new StateException("invalid_request", "device key fingerprint must be 64 hex characters");
        }
        return fingerprint.toLowerCase(Locale.ROOT);
    }

    // ================================================================== internals

    @FunctionalInterface
    private interface RemoteWork<T> {
        T run(Connection c, EffectivePolicy policy) throws SQLException;
    }

    private <T> T remote(RemoteActor actor, String action, String target, RemoteWork<T> work) {
        Objects.requireNonNull(actor, "actor");
        try {
            return db.write(c -> {
                EffectivePolicy policy = authz.resolve(c, actor.principalId(), actor.deviceId());
                if (!policy.isAuthenticated()) {
                    throw new StateException("forbidden", Decision.NO_IDENTITY);
                }
                return work.run(c, policy);
            });
        } catch (StateException e) {
            if ("forbidden".equals(e.getCode())) {
                auditRefusal(safeRemoteActor(actor), action, target, "denied", e);
            }
            throw e;
        }
    }

    private static void enforce(Decision decision) {
        if (!decision.allowed()) {
            throw new StateException("forbidden", decision.reason());
        }
    }

    /** Verifies purpose, freshness, lifetime and single use of a local-authority proof. */
    private LocalAuthority.Attestation consumeProof(Connection c, LocalAuthority.Proof proof,
                                                    LocalAuthority.Purpose purpose) throws SQLException {
        Optional<LocalAuthority.Attestation> verified = proof == null
                ? Optional.empty() : localAuthority.verify(proof, purpose);
        LocalAuthority.Attestation att = verified.orElseThrow(this::localRequired);
        if (att.purpose() != purpose || att.nonce() == null || att.nonce().length() < 16
                || att.nonce().length() > 256 || att.issuedAt() == null || att.expiresAt() == null) {
            throw localRequired();
        }
        try {
            StateValues.requireText("osActor", att.osActor(), 128);
        } catch (StateException e) {
            throw localRequired();
        }
        Instant now = Instant.ofEpochMilli(now());
        if (!att.expiresAt().isAfter(att.issuedAt()) // non-positive lifetime: expiry must follow issuance
                || att.issuedAt().isAfter(now.plus(PROOF_CLOCK_SKEW)) || !att.expiresAt().isAfter(now)
                || Duration.between(att.issuedAt(), att.expiresAt()).compareTo(MAX_PROOF_LIFETIME) > 0) {
            throw localRequired();
        }
        try (PreparedStatement ps = c.prepareStatement(
                "INSERT INTO local_authority_uses (nonce_hash, purpose, used_at) VALUES (?,?,?) "
                        + "ON CONFLICT(nonce_hash) DO NOTHING")) {
            ps.setString(1, StateValues.sha256Hex("vesta-local-proof-v1:" + att.nonce()));
            ps.setString(2, purpose.id());
            ps.setLong(3, now.toEpochMilli());
            if (ps.executeUpdate() != 1) {
                throw localRequired(); // replayed proof
            }
        }
        return att;
    }

    private StateException localRequired() {
        return new StateException("local_authority_required", "a valid local authority proof is required");
    }

    private BootstrapCode insertCode(Connection c, LocalAuthority.Purpose purpose, Duration life) throws SQLException {
        long now = now();
        try (PreparedStatement ps = c.prepareStatement(
                "UPDATE bootstrap_codes SET invalidated_at = ? WHERE consumed_at IS NULL AND invalidated_at IS NULL")) {
            ps.setLong(1, now);
            ps.executeUpdate();
        }
        byte[] bytes = new byte[CODE_BYTES];
        random.nextBytes(bytes);
        String code = Base64.getUrlEncoder().withoutPadding().encodeToString(bytes);
        long expires = now + life.toMillis();
        try (PreparedStatement ps = c.prepareStatement(
                "INSERT INTO bootstrap_codes (id, server_id, purpose, code_hash, created_at, expires_at) "
                        + "VALUES (?,?,?,?,?,?)")) {
            ps.setString(1, UUID.randomUUID().toString());
            ps.setString(2, db.serverId());
            ps.setString(3, purpose.id());
            ps.setString(4, StateValues.sha256Hex(code));
            ps.setLong(5, now);
            ps.setLong(6, expires);
            ps.executeUpdate();
        }
        return new BootstrapCode(code, purpose, Instant.ofEpochMilli(expires));
    }

    private String insertPrincipal(Connection c, String displayName) throws SQLException {
        String id = UUID.randomUUID().toString();
        long now = now();
        try (PreparedStatement ps = c.prepareStatement(
                "INSERT INTO principals (id, server_id, display_name, status, created_at, updated_at) "
                        + "VALUES (?,?,?,'active',?,?)")) {
            ps.setString(1, id);
            ps.setString(2, db.serverId());
            ps.setString(3, displayName);
            ps.setLong(4, now);
            ps.setLong(5, now);
            ps.executeUpdate();
        }
        return id;
    }

    private String insertDevice(Connection c, String principalId, String label, String fingerprint) throws SQLException {
        try (PreparedStatement ps = c.prepareStatement("SELECT 1 FROM devices WHERE public_key_sha256 = ?")) {
            ps.setString(1, fingerprint);
            try (ResultSet rs = ps.executeQuery()) {
                if (rs.next()) {
                    throw new StateException("conflict", "device key is already registered");
                }
            }
        }
        String id = UUID.randomUUID().toString();
        try (PreparedStatement ps = c.prepareStatement(
                "INSERT INTO devices (id, server_id, principal_id, label, public_key_sha256, created_at) "
                        + "VALUES (?,?,?,?,?,?)")) {
            ps.setString(1, id);
            ps.setString(2, db.serverId());
            ps.setString(3, principalId);
            ps.setString(4, label);
            ps.setString(5, fingerprint);
            ps.setLong(6, now());
            ps.executeUpdate();
        }
        return id;
    }

    /** One primary role: revokes any active assignment, then inserts the new one. */
    private String replacePrimaryRole(Connection c, String principalId, String roleId, Long expiresAt,
                                      String grantedBy) throws SQLException {
        long now = now();
        try (PreparedStatement ps = c.prepareStatement(
                "UPDATE role_assignments SET revoked_at = ?, revoked_by = ? WHERE principal_id = ? AND revoked_at IS NULL")) {
            ps.setLong(1, now);
            ps.setString(2, grantedBy);
            ps.setString(3, principalId);
            ps.executeUpdate();
        }
        String id = UUID.randomUUID().toString();
        try (PreparedStatement ps = c.prepareStatement(
                "INSERT INTO role_assignments (id, principal_id, role_id, granted_by, created_at, expires_at) "
                        + "VALUES (?,?,?,?,?,?)")) {
            ps.setString(1, id);
            ps.setString(2, principalId);
            ps.setString(3, roleId);
            ps.setString(4, grantedBy);
            ps.setLong(5, now);
            setNullableLong(ps, 6, expiresAt);
            ps.executeUpdate();
        }
        return id;
    }

    private boolean activeOwnerExists(Connection c) throws SQLException {
        try (PreparedStatement ps = c.prepareStatement(
                "SELECT 1 FROM role_assignments a JOIN principals p ON p.id = a.principal_id "
                        + "WHERE a.role_id = ? AND a.revoked_at IS NULL AND (a.expires_at IS NULL OR a.expires_at > ?) "
                        + "AND p.status = 'active' AND p.server_id = ? LIMIT 1")) {
            ps.setString(1, BuiltinRoles.OWNER);
            ps.setLong(2, now());
            ps.setString(3, db.serverId());
            try (ResultSet rs = ps.executeQuery()) {
                return rs.next();
            }
        }
    }

    private void requirePrincipal(Connection c, String principalId) throws SQLException {
        lookupPrincipalStatus(c, principalId);
    }

    private void requireActivePrincipal(Connection c, String principalId) throws SQLException {
        if (!"active".equals(lookupPrincipalStatus(c, principalId))) {
            throw new StateException("invalid_request", "principal is not active");
        }
    }

    private String lookupPrincipalStatus(Connection c, String principalId) throws SQLException {
        requireId("principalId", principalId);
        try (PreparedStatement ps = c.prepareStatement("SELECT status FROM principals WHERE id = ? AND server_id = ?")) {
            ps.setString(1, principalId);
            ps.setString(2, db.serverId());
            try (ResultSet rs = ps.executeQuery()) {
                if (!rs.next()) {
                    throw new StateException("not_found", "principal not found");
                }
                return rs.getString(1);
            }
        }
    }

    private Long expiry(Instant expiresAt) {
        if (expiresAt == null) {
            return null;
        }
        if (!expiresAt.isAfter(Instant.ofEpochMilli(now()))) {
            throw new StateException("invalid_request", "expiry must be in the future");
        }
        return expiresAt.toEpochMilli();
    }

    private static Duration codeTtl(Duration ttl) {
        Duration life = ttl == null ? DEFAULT_CODE_TTL : ttl;
        if (life.compareTo(MIN_CODE_TTL) < 0 || life.compareTo(MAX_CODE_TTL) > 0) {
            throw new StateException("invalid_request", "code lifetime must be between 1 and 60 minutes");
        }
        return life;
    }

    private static String requireId(String name, String value) {
        return StateValues.requireText(name, value, 128);
    }

    private static Map<String, String> metadata(Object... keyValues) {
        java.util.TreeMap<String, String> out = new java.util.TreeMap<>();
        for (int i = 0; i + 1 < keyValues.length; i += 2) {
            Object v = keyValues[i + 1];
            if (v != null) {
                out.put((String) keyValues[i], v.toString());
            }
        }
        return out;
    }

    private static void setNullableLong(PreparedStatement ps, int index, Long value) throws SQLException {
        if (value == null) {
            ps.setNull(index, java.sql.Types.INTEGER);
        } else {
            ps.setLong(index, value);
        }
    }

    private static AuditActor localActor() {
        return new AuditActor(SOURCE_LOCAL, AuditActor.LOCAL_DEVICE, SOURCE_LOCAL);
    }

    private static AuditActor remoteActor(EffectivePolicy policy) {
        return new AuditActor(policy.principalId(), policy.deviceId(), SOURCE_REMOTE);
    }

    private static AuditActor safeRemoteActor(RemoteActor actor) {
        try {
            return new AuditActor(actor.principalId(), actor.deviceId(), SOURCE_REMOTE);
        } catch (RuntimeException invalid) {
            return new AuditActor("unknown", "unknown", SOURCE_REMOTE);
        }
    }

    /**
     * Audits a refusal in its own transaction (the refused mutation has already rolled back). If the audit
     * cannot be stored, this never pretends it was: it throws {@code audit_unavailable} with the original
     * refusal as cause. Only bounded reason codes are recorded, never proofs, codes or nonces.
     */
    private void auditRefusal(AuditActor actor, String action, String target, String outcome,
                              StateException primary) {
        String reason = "forbidden".equals(primary.getCode()) ? primary.getMessage() : primary.getCode();
        try {
            audit.append(new AuditRecord(actor, action, safeTarget(target), outcome, null,
                    Map.of("reason", StateValues.bounded(reason == null ? "unknown" : reason, 64))));
        } catch (RuntimeException auditError) {
            log.warn("Could not audit refused {} ({}); reporting audit_unavailable", action, primary.getCode());
            StateException unavailable = new StateException("audit_unavailable",
                    "request refused (" + primary.getCode() + ") and the refusal could not be audited", primary);
            unavailable.addSuppressed(auditError);
            throw unavailable;
        }
    }

    /** Runs a local-proof operation, auditing refusals (bad/replayed proof, closed bootstrap, bad target). */
    private <T> T local(String action, String target, Supplier<T> work) {
        try {
            return work.get();
        } catch (StateException e) {
            if (LOCAL_REFUSALS.contains(e.getCode())) {
                auditRefusal(AuditActor.system(SOURCE_LOCAL), action, target, "rejected", e);
            }
            throw e;
        }
    }

    private static String safeTarget(String target) {
        try {
            return StateValues.requireText("target", target, 512);
        } catch (StateException invalid) {
            return "invalid-target";
        }
    }

    private long now() {
        return db.clock().millis();
    }
}
