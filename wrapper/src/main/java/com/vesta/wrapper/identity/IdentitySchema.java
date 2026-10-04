package com.vesta.wrapper.identity;

import com.vesta.wrapper.authorization.BuiltinRoles;
import com.vesta.wrapper.authorization.Capability;

import java.sql.Connection;
import java.sql.SQLException;
import java.sql.Statement;
import java.util.ArrayList;
import java.util.List;
import java.util.regex.Pattern;

/**
 * Isolated identity/authorization migration SQL. This class never touches {@code PRAGMA user_version}
 * and keeps no version metadata of its own: the central {@code StateDatabase} migration runner owns
 * versioning, the pre-migration backup and the surrounding transaction, and composes
 * {@link #apply(Connection, long)} into its next schema version.
 *
 * <p>Requires the foundation {@code server_identity} table. Built-in roles and their capability
 * presets are seeded here and protected by triggers; future preset changes need a new migration.
 */
public final class IdentitySchema {
    private static final Pattern SAFE_LITERAL = Pattern.compile("[a-z][a-z0-9._-]{0,63}");
    private static final Pattern SAFE_NAME = Pattern.compile("[A-Za-z][A-Za-z ]{0,31}");

    private IdentitySchema() {
    }

    /**
     * Executes the identity schema statements on {@code c}. The caller owns the transaction (and must
     * roll back on failure) and the schema version bump.
     */
    public static void apply(Connection c, long nowMillis) throws SQLException {
        try (Statement s = c.createStatement()) {
            for (String sql : statements(nowMillis)) {
                s.execute(sql);
            }
        }
    }

    /** The ordered DDL/seed statements, for composition into the central migration list. */
    public static List<String> statements(long nowMillis) {
        List<String> out = new ArrayList<>(List.of(
                """
                CREATE TABLE principals (
                    id TEXT PRIMARY KEY,
                    server_id TEXT NOT NULL REFERENCES server_identity(server_id),
                    display_name TEXT NOT NULL,
                    status TEXT NOT NULL CHECK (status IN ('active','disabled')),
                    created_at INTEGER NOT NULL,
                    updated_at INTEGER NOT NULL
                ) STRICT""",
                """
                CREATE TABLE devices (
                    id TEXT PRIMARY KEY,
                    server_id TEXT NOT NULL REFERENCES server_identity(server_id),
                    principal_id TEXT NOT NULL REFERENCES principals(id),
                    label TEXT NOT NULL,
                    public_key_sha256 TEXT NOT NULL UNIQUE CHECK (length(public_key_sha256) = 64),
                    created_at INTEGER NOT NULL,
                    revoked_at INTEGER,
                    revoked_by TEXT
                ) STRICT""",
                "CREATE INDEX devices_principal ON devices(principal_id)",
                """
                CREATE TABLE roles (
                    id TEXT PRIMARY KEY,
                    name TEXT NOT NULL UNIQUE COLLATE NOCASE,
                    rank INTEGER NOT NULL CHECK (rank BETWEEN 1 AND 100),
                    builtin INTEGER NOT NULL CHECK (builtin IN (0,1)),
                    created_by TEXT NOT NULL,
                    created_at INTEGER NOT NULL,
                    -- custom roles can never reach the locally granted Administrator/Owner tier
                    CHECK (builtin = 1 OR rank < 80)
                ) STRICT""",
                """
                CREATE TABLE role_capabilities (
                    role_id TEXT NOT NULL REFERENCES roles(id),
                    capability TEXT NOT NULL,
                    scope TEXT NOT NULL,
                    PRIMARY KEY (role_id, capability, scope)
                ) STRICT""",
                """
                CREATE TABLE role_assignments (
                    id TEXT PRIMARY KEY,
                    principal_id TEXT NOT NULL REFERENCES principals(id),
                    role_id TEXT NOT NULL REFERENCES roles(id),
                    granted_by TEXT NOT NULL,
                    created_at INTEGER NOT NULL,
                    expires_at INTEGER,
                    revoked_at INTEGER,
                    revoked_by TEXT,
                    expiry_recorded_at INTEGER
                ) STRICT""",
                "CREATE INDEX role_assignments_principal ON role_assignments(principal_id)",
                """
                CREATE TABLE principal_overrides (
                    id TEXT PRIMARY KEY,
                    principal_id TEXT NOT NULL REFERENCES principals(id),
                    capability TEXT NOT NULL,
                    scope TEXT NOT NULL,
                    effect TEXT NOT NULL CHECK (effect IN ('allow','deny')),
                    granted_by TEXT NOT NULL,
                    created_at INTEGER NOT NULL,
                    expires_at INTEGER,
                    revoked_at INTEGER,
                    revoked_by TEXT,
                    expiry_recorded_at INTEGER
                ) STRICT""",
                "CREATE INDEX principal_overrides_principal ON principal_overrides(principal_id)",
                """
                CREATE TABLE bootstrap_codes (
                    id TEXT PRIMARY KEY,
                    server_id TEXT NOT NULL REFERENCES server_identity(server_id),
                    purpose TEXT NOT NULL CHECK (purpose IN ('owner_bootstrap','owner_recovery')),
                    code_hash TEXT NOT NULL UNIQUE CHECK (length(code_hash) = 64),
                    created_at INTEGER NOT NULL,
                    expires_at INTEGER NOT NULL,
                    consumed_at INTEGER,
                    invalidated_at INTEGER
                ) STRICT""",
                """
                CREATE TABLE local_authority_uses (
                    nonce_hash TEXT PRIMARY KEY CHECK (length(nonce_hash) = 64),
                    purpose TEXT NOT NULL,
                    used_at INTEGER NOT NULL
                ) STRICT"""));

        for (BuiltinRoles.Preset preset : BuiltinRoles.presets()) {
            String id = literal(preset.id());
            if (!SAFE_NAME.matcher(preset.name()).matches()) {
                throw new IllegalStateException("unsafe builtin role name");
            }
            out.add("INSERT INTO roles (id, name, rank, builtin, created_by, created_at) VALUES ('" + id + "', '"
                    + preset.name() + "', " + preset.rank() + ", 1, 'migration', " + nowMillis + ")");
            for (Capability capability : preset.capabilities()) {
                out.add("INSERT INTO role_capabilities (role_id, capability, scope) VALUES ('" + id + "', '"
                        + literal(capability.id()) + "', '*')");
            }
        }

        // Built-in presets are immutable once seeded (created after seeding on purpose).
        out.add("""
                CREATE TRIGGER roles_builtin_no_update BEFORE UPDATE ON roles WHEN OLD.builtin = 1 OR NEW.builtin = 1
                BEGIN SELECT RAISE(ABORT, 'builtin roles are immutable'); END""");
        out.add("""
                CREATE TRIGGER roles_builtin_no_delete BEFORE DELETE ON roles WHEN OLD.builtin = 1
                BEGIN SELECT RAISE(ABORT, 'builtin roles are immutable'); END""");
        out.add("""
                CREATE TRIGGER roles_builtin_no_insert BEFORE INSERT ON roles WHEN NEW.builtin = 1
                BEGIN SELECT RAISE(ABORT, 'builtin roles are immutable'); END""");
        out.add("""
                CREATE TRIGGER role_capabilities_builtin_no_insert BEFORE INSERT ON role_capabilities
                WHEN (SELECT builtin FROM roles WHERE id = NEW.role_id) = 1
                BEGIN SELECT RAISE(ABORT, 'builtin roles are immutable'); END""");
        out.add("""
                CREATE TRIGGER role_capabilities_builtin_no_update BEFORE UPDATE ON role_capabilities
                WHEN (SELECT builtin FROM roles WHERE id = OLD.role_id) = 1
                  OR (SELECT builtin FROM roles WHERE id = NEW.role_id) = 1
                BEGIN SELECT RAISE(ABORT, 'builtin roles are immutable'); END""");
        out.add("""
                CREATE TRIGGER role_capabilities_builtin_no_delete BEFORE DELETE ON role_capabilities
                WHEN (SELECT builtin FROM roles WHERE id = OLD.role_id) = 1
                BEGIN SELECT RAISE(ABORT, 'builtin roles are immutable'); END""");
        // Replay records for local-authority proofs are append-only.
        out.add("""
                CREATE TRIGGER local_authority_uses_no_update BEFORE UPDATE ON local_authority_uses
                BEGIN SELECT RAISE(ABORT, 'local_authority_uses is append-only'); END""");
        out.add("""
                CREATE TRIGGER local_authority_uses_no_delete BEFORE DELETE ON local_authority_uses
                BEGIN SELECT RAISE(ABORT, 'local_authority_uses is append-only'); END""");
        return List.copyOf(out);
    }

    private static String literal(String value) {
        if (!SAFE_LITERAL.matcher(value).matches()) {
            throw new IllegalStateException("unsafe seed literal");
        }
        return value;
    }
}
