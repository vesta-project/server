//! Forward-only SQLite schema migrations. The database runner owns the transaction and versioning.
use rusqlite::{Connection, params};

/// Applies one fixed schema migration without managing the surrounding transaction or schema version.
pub(crate) fn apply(c: &Connection, version: u32, now_ms: i64) -> crate::Result<()> {
    match version {
        1 => apply_v1(c, now_ms),
        2 => apply_v2(c),
        3 => apply_v3(c, now_ms),
        4 => apply_v4(c),
        5 => apply_v5(c),
        6 => apply_v6(c),
        _ => Err(crate::StateError::new("state_migration_unknown")),
    }
}

fn apply_v1(c: &Connection, now_ms: i64) -> crate::Result<()> {
    let server_id = uuid::Uuid::new_v4().to_string();
    c.execute_batch(
        r#"
        CREATE TABLE meta (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL
        ) STRICT;
        CREATE TABLE server_identity (
            singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
            server_id TEXT NOT NULL UNIQUE,
            created_at INTEGER NOT NULL
        ) STRICT;
        "#,
    )?;
    c.execute(
        "INSERT INTO server_identity (singleton, server_id, created_at) VALUES (1, ?1, ?2)",
        params![server_id, now_ms],
    )?;
    c.execute_batch(
        r#"
        INSERT INTO meta (key, value) VALUES ('events_pruned_through', '0');
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
        ) STRICT;
        CREATE INDEX operations_state_seq ON operations(state, seq);
        CREATE TABLE operation_resources (
            operation_id TEXT NOT NULL REFERENCES operations(id) ON DELETE CASCADE,
            resource TEXT NOT NULL,
            PRIMARY KEY (operation_id, resource)
        ) STRICT;
        CREATE TABLE resource_locks (
            resource TEXT PRIMARY KEY,
            operation_id TEXT NOT NULL REFERENCES operations(id),
            acquired_at INTEGER NOT NULL
        ) STRICT;
        CREATE INDEX resource_locks_operation ON resource_locks(operation_id);
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
        ) STRICT;
        CREATE INDEX confirmations_operation ON confirmations(operation_id);
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
        ) STRICT;
        CREATE TRIGGER audit_events_no_update BEFORE UPDATE ON audit_events
        BEGIN SELECT RAISE(ABORT, 'audit_events is append-only'); END;
        CREATE TRIGGER audit_events_no_delete BEFORE DELETE ON audit_events
        BEGIN SELECT RAISE(ABORT, 'audit_events is append-only'); END;
        CREATE TABLE events (
            cursor INTEGER PRIMARY KEY AUTOINCREMENT,
            server_id TEXT NOT NULL REFERENCES server_identity(server_id),
            type TEXT NOT NULL,
            operation_id TEXT,
            at INTEGER NOT NULL,
            payload_json TEXT NOT NULL
        ) STRICT;
        CREATE INDEX events_at ON events(at);
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
        ) STRICT;
        "#,
    )?;
    Ok(())
}

fn apply_v2(c: &Connection) -> crate::Result<()> {
    c.execute_batch(
        r#"
        ALTER TABLE operations ADD COLUMN input_json TEXT
            CHECK (input_json IS NULL OR length(input_json) <= 65536);
        ALTER TABLE operations ADD COLUMN input_status TEXT NOT NULL DEFAULT 'unavailable_legacy'
            CHECK (input_status IN ('available','unavailable_legacy','purged'));
        ALTER TABLE operations ADD COLUMN owner_token TEXT;
        CREATE INDEX operations_input_status ON operations(input_status, state);
        "#,
    )?;
    Ok(())
}

fn apply_v3(c: &Connection, now_ms: i64) -> crate::Result<()> {
    c.execute_batch(
        r#"
        CREATE TABLE principals (
            id TEXT PRIMARY KEY,
            server_id TEXT NOT NULL REFERENCES server_identity(server_id),
            display_name TEXT NOT NULL,
            status TEXT NOT NULL CHECK (status IN ('active','disabled')),
            created_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL
        ) STRICT;
        CREATE TABLE devices (
            id TEXT PRIMARY KEY,
            server_id TEXT NOT NULL REFERENCES server_identity(server_id),
            principal_id TEXT NOT NULL REFERENCES principals(id),
            label TEXT NOT NULL,
            public_key_sha256 TEXT NOT NULL UNIQUE CHECK (length(public_key_sha256) = 64),
            created_at INTEGER NOT NULL,
            revoked_at INTEGER,
            revoked_by TEXT
        ) STRICT;
        CREATE INDEX devices_principal ON devices(principal_id);
        CREATE TABLE roles (
            id TEXT PRIMARY KEY,
            name TEXT NOT NULL UNIQUE COLLATE NOCASE,
            rank INTEGER NOT NULL CHECK (rank BETWEEN 1 AND 100),
            builtin INTEGER NOT NULL CHECK (builtin IN (0,1)),
            created_by TEXT NOT NULL,
            created_at INTEGER NOT NULL,
            -- custom roles can never reach the locally granted Administrator/Owner tier
            CHECK (builtin = 1 OR rank < 80)
        ) STRICT;
        CREATE TABLE role_capabilities (
            role_id TEXT NOT NULL REFERENCES roles(id),
            capability TEXT NOT NULL,
            scope TEXT NOT NULL,
            PRIMARY KEY (role_id, capability, scope)
        ) STRICT;
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
        ) STRICT;
        CREATE INDEX role_assignments_principal ON role_assignments(principal_id);
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
        ) STRICT;
        CREATE INDEX principal_overrides_principal ON principal_overrides(principal_id);
        CREATE TABLE bootstrap_codes (
            id TEXT PRIMARY KEY,
            server_id TEXT NOT NULL REFERENCES server_identity(server_id),
            purpose TEXT NOT NULL CHECK (purpose IN ('owner_bootstrap','owner_recovery')),
            code_hash TEXT NOT NULL UNIQUE CHECK (length(code_hash) = 64),
            created_at INTEGER NOT NULL,
            expires_at INTEGER NOT NULL,
            consumed_at INTEGER,
            invalidated_at INTEGER
        ) STRICT;
        CREATE TABLE local_authority_uses (
            nonce_hash TEXT PRIMARY KEY CHECK (length(nonce_hash) = 64),
            purpose TEXT NOT NULL,
            used_at INTEGER NOT NULL
        ) STRICT;
        "#,
    )?;

    for preset in BUILTIN_PRESETS {
        c.execute(
            "INSERT INTO roles (id, name, rank, builtin, created_by, created_at) VALUES (?1, ?2, ?3, 1, 'migration', ?4)",
            params![preset.id, preset.name, preset.rank, now_ms],
        )?;
        for capability in preset.capabilities {
            c.execute(
                "INSERT INTO role_capabilities (role_id, capability, scope) VALUES (?1, ?2, '*')",
                params![preset.id, capability],
            )?;
        }
    }

    // These protections are installed only after every built-in row and capability is seeded.
    c.execute_batch(
        r#"
        CREATE TRIGGER roles_builtin_no_update BEFORE UPDATE ON roles WHEN OLD.builtin = 1 OR NEW.builtin = 1
        BEGIN SELECT RAISE(ABORT, 'builtin roles are immutable'); END;
        CREATE TRIGGER roles_builtin_no_delete BEFORE DELETE ON roles WHEN OLD.builtin = 1
        BEGIN SELECT RAISE(ABORT, 'builtin roles are immutable'); END;
        CREATE TRIGGER roles_builtin_no_insert BEFORE INSERT ON roles WHEN NEW.builtin = 1
        BEGIN SELECT RAISE(ABORT, 'builtin roles are immutable'); END;
        CREATE TRIGGER role_capabilities_builtin_no_insert BEFORE INSERT ON role_capabilities
        WHEN (SELECT builtin FROM roles WHERE id = NEW.role_id) = 1
        BEGIN SELECT RAISE(ABORT, 'builtin roles are immutable'); END;
        CREATE TRIGGER role_capabilities_builtin_no_update BEFORE UPDATE ON role_capabilities
        WHEN (SELECT builtin FROM roles WHERE id = OLD.role_id) = 1
          OR (SELECT builtin FROM roles WHERE id = NEW.role_id) = 1
        BEGIN SELECT RAISE(ABORT, 'builtin roles are immutable'); END;
        CREATE TRIGGER role_capabilities_builtin_no_delete BEFORE DELETE ON role_capabilities
        WHEN (SELECT builtin FROM roles WHERE id = OLD.role_id) = 1
        BEGIN SELECT RAISE(ABORT, 'builtin roles are immutable'); END;
        CREATE TRIGGER local_authority_uses_no_update BEFORE UPDATE ON local_authority_uses
        BEGIN SELECT RAISE(ABORT, 'local_authority_uses is append-only'); END;
        CREATE TRIGGER local_authority_uses_no_delete BEFORE DELETE ON local_authority_uses
        BEGIN SELECT RAISE(ABORT, 'local_authority_uses is append-only'); END;
        "#,
    )?;
    Ok(())
}

fn apply_v4(c: &Connection) -> crate::Result<()> {
    c.execute_batch(
        r#"
        CREATE TABLE pack_passwords (
            server_id TEXT PRIMARY KEY REFERENCES server_identity(server_id),
            revision INTEGER NOT NULL CHECK (revision >= 1),
            phc TEXT CHECK (phc IS NULL OR length(phc) BETWEEN 1 AND 256),
            created_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL,
            revoked_at INTEGER,
            CHECK ((revoked_at IS NULL AND phc IS NOT NULL)
                OR (revoked_at IS NOT NULL AND phc IS NULL))
        ) STRICT;
        "#,
    )?;
    Ok(())
}

fn apply_v5(c: &Connection) -> crate::Result<()> {
    c.execute_batch(
        "CREATE INDEX role_assignments_pending_expiry ON role_assignments(expires_at, id) \
         WHERE expires_at IS NOT NULL AND revoked_at IS NULL AND expiry_recorded_at IS NULL;\
         CREATE INDEX principal_overrides_pending_expiry ON principal_overrides(expires_at, id) \
         WHERE expires_at IS NOT NULL AND revoked_at IS NULL AND expiry_recorded_at IS NULL;",
    )?;
    Ok(())
}

fn apply_v6(c: &Connection) -> crate::Result<()> {
    // Legacy consumed tokens include refusals: never backfill them as accepted proof.
    c.execute_batch(
        "ALTER TABLE confirmations ADD COLUMN accepted_at INTEGER \
         CHECK (accepted_at IS NULL OR (consumed_at IS NOT NULL AND \
         consumed_at=accepted_at AND accepted_at>=created_at AND accepted_at<expires_at));",
    )?;
    Ok(())
}

struct BuiltinPreset {
    id: &'static str,
    name: &'static str,
    rank: i64,
    capabilities: &'static [&'static str],
}

const BUILTIN_PRESETS: &[BuiltinPreset] = &[
    BuiltinPreset {
        id: "owner",
        name: "Owner",
        rank: 100,
        capabilities: &[
            "status.view",
            "status.detailed",
            "metrics.view",
            "players.view",
            "activity.view",
            "tasks.view",
            "diagnostics.view",
            "logs.read",
            "console.read",
            "console.write",
            "players.moderate",
            "lifecycle.start",
            "lifecycle.stop",
            "lifecycle.restart",
            "lifecycle.force_kill",
            "maintenance.toggle",
            "files.list",
            "files.read",
            "files.create",
            "files.overwrite",
            "files.rename",
            "files.mkdir",
            "files.delete",
            "content.install",
            "backups.view",
            "backups.create",
            "backups.verify",
            "backups.restore",
            "recovery.export",
            "schedules.view",
            "schedules.edit",
            "schedules.run",
            "config.view",
            "config.edit",
            "integrations.edit",
            "pack.access",
            "pack.edit",
            "pack.build",
            "pack.publish",
            "pack.force_update",
            "principals.view",
            "principals.manage",
            "devices.revoke",
            "roles.create",
            "roles.assign",
            "roles.revoke",
            "credentials.manage",
            "security.manage",
            "audit.view",
        ],
    },
    BuiltinPreset {
        id: "administrator",
        name: "Administrator",
        rank: 80,
        capabilities: &[
            "status.view",
            "status.detailed",
            "metrics.view",
            "players.view",
            "activity.view",
            "tasks.view",
            "diagnostics.view",
            "logs.read",
            "console.read",
            "console.write",
            "players.moderate",
            "lifecycle.start",
            "lifecycle.stop",
            "lifecycle.restart",
            "lifecycle.force_kill",
            "maintenance.toggle",
            "files.list",
            "files.read",
            "files.create",
            "files.overwrite",
            "files.rename",
            "files.mkdir",
            "files.delete",
            "content.install",
            "backups.view",
            "backups.create",
            "backups.verify",
            "schedules.view",
            "schedules.edit",
            "schedules.run",
            "config.view",
            "config.edit",
            "integrations.edit",
            "pack.access",
            "pack.edit",
            "pack.build",
            "pack.publish",
            "pack.force_update",
            "principals.view",
            "principals.manage",
            "devices.revoke",
            "roles.create",
            "roles.assign",
            "roles.revoke",
            "credentials.manage",
            "security.manage",
            "audit.view",
        ],
    },
    BuiltinPreset {
        id: "maintainer",
        name: "Maintainer",
        rank: 60,
        capabilities: &[
            "status.view",
            "status.detailed",
            "metrics.view",
            "players.view",
            "activity.view",
            "tasks.view",
            "diagnostics.view",
            "logs.read",
            "console.read",
            "console.write",
            "lifecycle.start",
            "lifecycle.stop",
            "lifecycle.restart",
            "lifecycle.force_kill",
            "maintenance.toggle",
            "files.list",
            "files.read",
            "files.create",
            "files.overwrite",
            "files.rename",
            "files.mkdir",
            "files.delete",
            "content.install",
            "backups.view",
            "backups.create",
            "backups.verify",
            "schedules.view",
            "schedules.edit",
            "schedules.run",
        ],
    },
    BuiltinPreset {
        id: "moderator",
        name: "Moderator",
        rank: 40,
        capabilities: &["status.view", "players.view", "players.moderate"],
    },
    BuiltinPreset {
        id: "viewer",
        name: "Viewer",
        rank: 20,
        capabilities: &[
            "status.view",
            "status.detailed",
            "metrics.view",
            "players.view",
            "activity.view",
            "tasks.view",
        ],
    },
    BuiltinPreset {
        id: "pack-user",
        name: "Pack User",
        rank: 10,
        capabilities: &["status.view", "pack.access"],
    },
];
