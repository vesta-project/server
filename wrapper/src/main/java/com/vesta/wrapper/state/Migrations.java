package com.vesta.wrapper.state;

import java.util.List;
import java.util.UUID;

/** Versioned, forward-only schema migrations. Each version runs in one transaction. */
final class Migrations {
    private Migrations() {
    }

    static List<String> statements(int version, long nowMillis) {
        return switch (version) {
            case 1 -> v1(nowMillis);
            case 2 -> v2();
            // v3: identity/authorization tables owned by the identity package; versioned only here.
            case 3 -> com.vesta.wrapper.identity.IdentitySchema.statements(nowMillis);
            default -> throw new StateException("state_migration_unknown", "No migration for schema " + version);
        };
    }

    /**
     * v2: persist bounded canonical operation inputs and runner ownership tokens. Rows created
     * before v2 only have a payload hash, so they are marked {@code unavailable_legacy}; inputs are
     * never fabricated and such rows are never executed.
     */
    private static List<String> v2() {
        return List.of(
                "ALTER TABLE operations ADD COLUMN input_json TEXT "
                        + "CHECK (input_json IS NULL OR length(input_json) <= 65536)",
                "ALTER TABLE operations ADD COLUMN input_status TEXT NOT NULL DEFAULT 'unavailable_legacy' "
                        + "CHECK (input_status IN ('available','unavailable_legacy','purged'))",
                "ALTER TABLE operations ADD COLUMN owner_token TEXT",
                "CREATE INDEX operations_input_status ON operations(input_status, state)");
    }

    private static List<String> v1(long now) {
        String serverId = UUID.randomUUID().toString();
        return List.of(
                """
                CREATE TABLE meta (
                    key TEXT PRIMARY KEY,
                    value TEXT NOT NULL
                ) STRICT""",
                """
                CREATE TABLE server_identity (
                    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
                    server_id TEXT NOT NULL UNIQUE,
                    created_at INTEGER NOT NULL
                ) STRICT""",
                "INSERT INTO server_identity (singleton, server_id, created_at) VALUES (1, '" + serverId + "', " + now + ")",
                "INSERT INTO meta (key, value) VALUES ('events_pruned_through', '0')",
                """
                CREATE TABLE operations (
                    seq INTEGER PRIMARY KEY AUTOINCREMENT,
                    id TEXT NOT NULL UNIQUE,
                    server_id TEXT NOT NULL REFERENCES server_identity(server_id),
                    actor_principal_id TEXT NOT NULL,
                    device_id TEXT NOT NULL,
                    source TEXT NOT NULL,
                    type TEXT NOT NULL,
                    target TEXT NOT NULL,
                    payload_hash TEXT NOT NULL,
                    idempotency_key TEXT,
                    state TEXT NOT NULL CHECK (state IN ('queued','validating','awaiting_confirmation','running',
                        'cancelling','succeeded','failed','cancelled','recovery_required')),
                    requires_confirmation INTEGER NOT NULL CHECK (requires_confirmation IN (0,1)),
                    progress_percent INTEGER NOT NULL DEFAULT 0 CHECK (progress_percent BETWEEN 0 AND 100),
                    progress_message TEXT,
                    cancel_requested INTEGER NOT NULL DEFAULT 0 CHECK (cancel_requested IN (0,1)),
                    external_effect_started INTEGER NOT NULL DEFAULT 0 CHECK (external_effect_started IN (0,1)),
                    external_effect_idempotent INTEGER NOT NULL CHECK (external_effect_idempotent IN (0,1)),
                    result_json TEXT,
                    error_code TEXT,
                    error_message TEXT,
                    created_at INTEGER NOT NULL,
                    updated_at INTEGER NOT NULL,
                    finished_at INTEGER,
                    UNIQUE (server_id, actor_principal_id, idempotency_key)
                ) STRICT""",
                "CREATE INDEX operations_state_seq ON operations(state, seq)",
                """
                CREATE TABLE operation_resources (
                    operation_id TEXT NOT NULL REFERENCES operations(id) ON DELETE CASCADE,
                    resource TEXT NOT NULL,
                    PRIMARY KEY (operation_id, resource)
                ) STRICT""",
                """
                CREATE TABLE resource_locks (
                    resource TEXT PRIMARY KEY,
                    operation_id TEXT NOT NULL REFERENCES operations(id),
                    acquired_at INTEGER NOT NULL
                ) STRICT""",
                "CREATE INDEX resource_locks_operation ON resource_locks(operation_id)",
                """
                CREATE TABLE confirmations (
                    id TEXT PRIMARY KEY,
                    server_id TEXT NOT NULL REFERENCES server_identity(server_id),
                    operation_id TEXT NOT NULL REFERENCES operations(id),
                    actor_principal_id TEXT NOT NULL,
                    device_id TEXT NOT NULL,
                    action TEXT NOT NULL,
                    target TEXT NOT NULL,
                    payload_hash TEXT NOT NULL,
                    source_revision TEXT NOT NULL,
                    token_hash TEXT NOT NULL UNIQUE,
                    created_at INTEGER NOT NULL,
                    expires_at INTEGER NOT NULL,
                    consumed_at INTEGER
                ) STRICT""",
                "CREATE INDEX confirmations_operation ON confirmations(operation_id)",
                """
                CREATE TABLE audit_events (
                    seq INTEGER PRIMARY KEY AUTOINCREMENT,
                    server_id TEXT NOT NULL REFERENCES server_identity(server_id),
                    principal_id TEXT NOT NULL,
                    device_id TEXT NOT NULL,
                    source TEXT NOT NULL,
                    action TEXT NOT NULL,
                    target TEXT NOT NULL,
                    outcome TEXT NOT NULL,
                    operation_id TEXT,
                    at INTEGER NOT NULL,
                    metadata_json TEXT NOT NULL,
                    prev_hash TEXT NOT NULL,
                    hash TEXT NOT NULL UNIQUE
                ) STRICT""",
                """
                CREATE TRIGGER audit_events_no_update BEFORE UPDATE ON audit_events
                BEGIN SELECT RAISE(ABORT, 'audit_events is append-only'); END""",
                """
                CREATE TRIGGER audit_events_no_delete BEFORE DELETE ON audit_events
                BEGIN SELECT RAISE(ABORT, 'audit_events is append-only'); END""",
                """
                CREATE TABLE events (
                    cursor INTEGER PRIMARY KEY AUTOINCREMENT,
                    server_id TEXT NOT NULL REFERENCES server_identity(server_id),
                    type TEXT NOT NULL,
                    operation_id TEXT,
                    at INTEGER NOT NULL,
                    payload_json TEXT NOT NULL
                ) STRICT""",
                "CREATE INDEX events_at ON events(at)",
                """
                CREATE TABLE settings (
                    server_id TEXT NOT NULL REFERENCES server_identity(server_id),
                    key TEXT NOT NULL,
                    desired_value TEXT NOT NULL,
                    desired_revision INTEGER NOT NULL CHECK (desired_revision >= 1),
                    apply_mode TEXT NOT NULL CHECK (apply_mode IN ('immediate','on_restart','manual')),
                    effective_value TEXT,
                    effective_revision INTEGER NOT NULL DEFAULT 0 CHECK (effective_revision >= 0),
                    updated_at INTEGER NOT NULL,
                    PRIMARY KEY (server_id, key)
                ) STRICT""");
    }
}
