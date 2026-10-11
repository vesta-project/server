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
        7 => apply_v7(c),
        8 => apply_v8(c),
        9 => apply_v9(c),
        10 => apply_v10(c),
        11 => apply_v11(c),
        12 => apply_v12(c),
        13 => apply_v13(c),
        14 => apply_v14(c),
        15 => apply_v15(c, now_ms),
        16 => apply_v16(c),
        17 => apply_v17(c, now_ms),
        _ => Err(crate::StateError::new("state_migration_unknown")),
    }
}

/// Preserve existing release rows while admitting native platform-reference artifacts.
fn apply_v10(c: &Connection) -> crate::Result<()> {
    c.execute_batch(r#"
        ALTER TABLE pack_releases RENAME TO pack_releases_v9;
        CREATE TABLE pack_releases (
            server_id TEXT NOT NULL REFERENCES server_identity(server_id),
            revision INTEGER NOT NULL CHECK (revision BETWEEN 1 AND 9007199254740991),
            minimum_revision INTEGER NOT NULL CHECK (minimum_revision BETWEEN 1 AND revision),
            display_version TEXT NOT NULL CHECK (length(CAST(display_version AS BLOB)) BETWEEN 1 AND 256),
            format TEXT NOT NULL CHECK (format IN ('vestapack', 'zip', 'mrpack', 'catalogue')),
            sha256 TEXT NOT NULL CHECK (length(sha256) = 64 AND sha256 NOT GLOB '*[^0-9a-f]*'),
            size INTEGER NOT NULL CHECK (size >= 0),
            published_at INTEGER NOT NULL,
            PRIMARY KEY (server_id, revision)
        ) STRICT;
        INSERT INTO pack_releases SELECT * FROM pack_releases_v9;
        DROP TABLE pack_releases_v9;
    "#)?;
    Ok(())
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

/// Every publication is one immutable row; the highest revision is the hosted release.
/// The same artifact may back several revisions (for example a republish or rollback).
fn apply_v7(c: &Connection) -> crate::Result<()> {
    c.execute_batch(
        r#"
        CREATE TABLE pack_releases (
            server_id TEXT NOT NULL REFERENCES server_identity(server_id),
            revision INTEGER NOT NULL CHECK (revision BETWEEN 1 AND 9007199254740991),
            minimum_revision INTEGER NOT NULL CHECK (minimum_revision BETWEEN 1 AND revision),
            display_version TEXT NOT NULL CHECK (length(CAST(display_version AS BLOB)) BETWEEN 1 AND 256),
            format TEXT NOT NULL CHECK (format IN ('vestapack', 'zip', 'mrpack')),
            sha256 TEXT NOT NULL CHECK (length(sha256) = 64 AND sha256 NOT GLOB '*[^0-9a-f]*'),
            size INTEGER NOT NULL CHECK (size >= 0),
            published_at INTEGER NOT NULL,
            PRIMARY KEY (server_id, revision)
        ) STRICT;
        "#,
    )?;
    Ok(())
}

/// Recoverable copies of replaced or deleted game files. Bytes live in `<state>/trash`
/// under `name`; rows are removed when the copy is restored, purged or expires.
fn apply_v8(c: &Connection) -> crate::Result<()> {
    c.execute_batch(
        r#"
        CREATE TABLE file_trash (
            id TEXT PRIMARY KEY CHECK (length(id) = 32 AND id NOT GLOB '*[^0-9a-f]*'),
            server_id TEXT NOT NULL REFERENCES server_identity(server_id),
            path TEXT NOT NULL CHECK (length(CAST(path AS BLOB)) BETWEEN 1 AND 4096),
            sha256 TEXT NOT NULL CHECK (length(sha256) = 64 AND sha256 NOT GLOB '*[^0-9a-f]*'),
            size INTEGER NOT NULL CHECK (size >= 0),
            reason TEXT NOT NULL CHECK (reason IN ('replace', 'delete')),
            principal_id TEXT NOT NULL,
            device_id TEXT NOT NULL,
            status TEXT NOT NULL CHECK (status IN ('pending', 'complete', 'recovery-required', 'restored')),
            effect_sha256 TEXT CHECK (effect_sha256 IS NULL OR (length(effect_sha256) = 64 AND effect_sha256 NOT GLOB '*[^0-9a-f]*')),
            unix_mode INTEGER CHECK (unix_mode IS NULL OR unix_mode BETWEEN 0 AND 4095),
            readonly INTEGER NOT NULL CHECK (readonly IN (0, 1)),
            trashed_at INTEGER NOT NULL,
            expires_at INTEGER NOT NULL CHECK (expires_at > trashed_at),
            CHECK ((reason = 'replace' AND effect_sha256 IS NOT NULL) OR (reason = 'delete' AND effect_sha256 IS NULL))
        ) STRICT;
        CREATE INDEX file_trash_expiry ON file_trash(server_id, expires_at);
        "#,
    )?;
    Ok(())
}

/// Bounded resumable upload reservations. Byte staging lives in `<state>/uploads`.
fn apply_v9(c: &Connection) -> crate::Result<()> {
    c.execute_batch(
        r#"
        CREATE TABLE file_uploads (
            id TEXT PRIMARY KEY CHECK (length(id) = 32 AND id NOT GLOB '*[^0-9a-f]*'),
            server_id TEXT NOT NULL REFERENCES server_identity(server_id),
            principal_id TEXT NOT NULL,
            device_id TEXT NOT NULL,
            path TEXT NOT NULL CHECK (length(CAST(path AS BLOB)) BETWEEN 1 AND 4096),
            expected_sha256 TEXT CHECK (expected_sha256 IS NULL OR (length(expected_sha256) = 64 AND expected_sha256 NOT GLOB '*[^0-9a-f]*')),
            total_size INTEGER NOT NULL CHECK (total_size BETWEEN 0 AND 16777216),
            final_sha256 TEXT NOT NULL CHECK (length(final_sha256) = 64 AND final_sha256 NOT GLOB '*[^0-9a-f]*'),
            expires_at INTEGER NOT NULL
        ) STRICT;
        "#,
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

/// Durable file mutation intents, independent of operation scheduler retention.
fn apply_v11(c: &Connection) -> crate::Result<()> {
    c.execute_batch(r#"
        CREATE TABLE file_intents (
            id TEXT PRIMARY KEY CHECK (length(id)=32 AND id NOT GLOB '*[^0-9a-f]*'),
            server_id TEXT NOT NULL REFERENCES server_identity(server_id),
            principal_id TEXT NOT NULL,
            device_id TEXT NOT NULL,
            kind TEXT NOT NULL CHECK (kind IN ('write','delete','mkdir','rename','restore')),
            path TEXT NOT NULL CHECK (length(CAST(path AS BLOB)) BETWEEN 1 AND 4096),
            details_json TEXT NOT NULL CHECK (json_valid(details_json) AND length(CAST(details_json AS BLOB))<=32768),
            status TEXT NOT NULL CHECK (status IN ('pending','complete','aborted','recovery-required')),
            created_at INTEGER NOT NULL
        ) STRICT;
        CREATE INDEX file_intents_pending ON file_intents(server_id,status);
    "#)?;
    Ok(())
}

/// P2 credentials are separate from both legacy bootstrap and pack access.
fn apply_v12(c: &Connection) -> crate::Result<()> {
    c.execute_batch(
        r#"
        CREATE TABLE management_pairing_codes (
            code_hash BLOB PRIMARY KEY CHECK (length(code_hash) = 32),
            server_id TEXT NOT NULL REFERENCES server_identity(server_id),
            role_id TEXT NOT NULL REFERENCES roles(id),
            created_at INTEGER NOT NULL,
            expires_at INTEGER NOT NULL CHECK (expires_at > created_at)
        ) STRICT;
        DROP TRIGGER role_capabilities_builtin_no_insert;
        INSERT OR IGNORE INTO role_capabilities (role_id,capability,scope) SELECT id,'public.info','*' FROM roles WHERE id IN ('owner','administrator','pack-user');
        INSERT OR IGNORE INTO role_capabilities (role_id,capability,scope) SELECT id,'pack.download','*' FROM roles WHERE id IN ('owner','administrator','pack-user');
        INSERT OR IGNORE INTO role_capabilities (role_id,capability,scope) SELECT id,'pack.realtime','*' FROM roles WHERE id IN ('owner','administrator','pack-user');
        CREATE TRIGGER role_capabilities_builtin_no_insert BEFORE INSERT ON role_capabilities
        WHEN (SELECT builtin FROM roles WHERE id = NEW.role_id) = 1
        BEGIN SELECT RAISE(ABORT, 'builtin roles are immutable'); END;
        CREATE TABLE user_minecraft_accounts (
            server_id TEXT NOT NULL REFERENCES server_identity(server_id),
            minecraft_uuid TEXT NOT NULL,
            principal_id TEXT NOT NULL REFERENCES principals(id),
            PRIMARY KEY(server_id,minecraft_uuid), UNIQUE(server_id,principal_id)
        ) STRICT;
        CREATE TABLE user_default_roles (
            principal_id TEXT PRIMARY KEY REFERENCES principals(id),
            role_id TEXT NOT NULL REFERENCES roles(id)
        ) STRICT;
        CREATE TABLE device_password_grants (
            device_id TEXT PRIMARY KEY REFERENCES devices(id),
            role_id TEXT NOT NULL REFERENCES roles(id),
            password_revision INTEGER NOT NULL CHECK(password_revision > 0)
        ) STRICT;
        CREATE TABLE access_role_sources (
            singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
            settings_json TEXT NOT NULL CHECK(length(settings_json) <= 4096)
        ) STRICT;
        CREATE TABLE management_owner_links (
            server_id TEXT NOT NULL REFERENCES server_identity(server_id),
            minecraft_uuid TEXT NOT NULL,
            role_id TEXT NOT NULL REFERENCES roles(id),
            created_at INTEGER NOT NULL,
            PRIMARY KEY(server_id, minecraft_uuid)
        ) STRICT;
        CREATE TABLE management_device_keys (
            device_id TEXT PRIMARY KEY REFERENCES devices(id),
            server_id TEXT NOT NULL REFERENCES server_identity(server_id),
            public_key BLOB NOT NULL CHECK (length(public_key) = 32),
            linked_minecraft_uuid TEXT
        ) STRICT;
    "#,
    )?;
    // Existing custom roles may use either the preferred names or IDs. Never
    // adopt them as automatic defaults: they may contain management authority.
    let public = seed_access_default(c, "public", "Public", 1, &["public.info"])?;
    let player = seed_access_default(
        c,
        "player",
        "Player",
        10,
        &["public.info", "pack.download", "pack.realtime"],
    )?;
    let settings = format!(
        r#"{{"anonymous":"{public}","new_user":"{public}","whitelist":"{player}","whitelist_enabled":true,"password":"{player}"}}"#
    );
    c.execute("INSERT INTO access_role_sources VALUES (1,?)", [settings])?;
    Ok(())
}

fn seed_access_default(
    c: &Connection,
    preferred_id: &str,
    name: &str,
    rank: u32,
    capabilities: &[&str],
) -> crate::Result<String> {
    let mut id = preferred_id.to_owned();
    let mut suffix = 0_u64;
    while c.query_row(
        "SELECT EXISTS(SELECT 1 FROM roles WHERE id=?)",
        [&id],
        |r| r.get::<_, bool>(0),
    )? {
        suffix += 1;
        id = format!("{preferred_id}-default-{suffix}");
    }
    let mut display = name.to_owned();
    suffix = 0;
    while c.query_row(
        "SELECT EXISTS(SELECT 1 FROM roles WHERE name=? COLLATE NOCASE)",
        [&display],
        |r| r.get::<_, bool>(0),
    )? {
        suffix += 1;
        display = format!("{name} (Vesta default {suffix})");
    }
    c.execute("INSERT INTO roles (id,name,rank,builtin,created_by,created_at) VALUES (?,?,?,0,'migration',0)",params![id,display,rank])?;
    for capability in capabilities {
        c.execute(
            "INSERT INTO role_capabilities VALUES (?,?,'*')",
            params![id, capability],
        )?;
    }
    Ok(id)
}

fn apply_v13(c: &Connection) -> crate::Result<()> {
    c.execute_batch(
        r#"
        CREATE TABLE management_pack_outcomes (
            server_id TEXT NOT NULL REFERENCES server_identity(server_id),
            principal_id TEXT NOT NULL,
            request_key TEXT NOT NULL CHECK(length(request_key) BETWEEN 1 AND 128),
            input_json TEXT NOT NULL CHECK(length(input_json) BETWEEN 1 AND 512),
            revision INTEGER NOT NULL CHECK(revision BETWEEN 1 AND 9007199254740991),
            minimum_revision INTEGER NOT NULL CHECK(minimum_revision BETWEEN 1 AND revision),
            PRIMARY KEY(server_id,principal_id,request_key)
        ) STRICT;
        CREATE TABLE pack_minimum_policy (
            server_id TEXT PRIMARY KEY REFERENCES server_identity(server_id),
            minimum_revision INTEGER NOT NULL CHECK(minimum_revision BETWEEN 1 AND 9007199254740991)
        ) STRICT;
    "#,
    )?;
    Ok(())
}

fn apply_v14(c: &Connection) -> crate::Result<()> {
    c.execute_batch(
        r#"
        CREATE TABLE management_file_outcomes (
            server_id TEXT NOT NULL REFERENCES server_identity(server_id),
            principal_id TEXT NOT NULL,
            device_id TEXT NOT NULL,
            request_key TEXT NOT NULL CHECK(length(request_key) BETWEEN 1 AND 128),
            input_sha256 TEXT NOT NULL CHECK(length(input_sha256)=64),
            authority_json TEXT NOT NULL CHECK(length(authority_json)<=16384),
            outcome_json TEXT CHECK(length(outcome_json)<=4096),
            PRIMARY KEY(server_id,principal_id,device_id,request_key)
        ) STRICT;
    "#,
    )?;
    Ok(())
}

fn apply_v15(c: &Connection, now_ms: i64) -> crate::Result<()> {
    c.execute_batch("ALTER TABLE management_file_outcomes ADD COLUMN completed_at INTEGER CHECK(completed_at IS NULL OR completed_at>=0); CREATE INDEX management_file_outcomes_retention ON management_file_outcomes(server_id,completed_at) WHERE outcome_json IS NOT NULL;")?;
    c.execute(
        "UPDATE management_file_outcomes SET completed_at=? WHERE outcome_json IS NOT NULL",
        [now_ms],
    )?;
    Ok(())
}

/// Retention for remote permission receipts. Existing receipts are dated at upgrade time
/// so a retry inside the retention window still replays rather than reapplying.
fn apply_v17(c: &Connection, now_ms: i64) -> crate::Result<()> {
    c.execute_batch("ALTER TABLE management_permission_receipts ADD COLUMN completed_at INTEGER NOT NULL DEFAULT 0 CHECK(completed_at>=0); CREATE INDEX management_permission_receipts_retention ON management_permission_receipts(completed_at);")?;
    c.execute(
        "UPDATE management_permission_receipts SET completed_at=?",
        [now_ms],
    )?;
    Ok(())
}

/// Durable bounded remote permission receipts and issuer-bound pairing codes.
fn apply_v16(c: &Connection) -> crate::Result<()> {
    c.execute_batch(r#"
        CREATE TABLE management_permission_receipts (
            key_hash BLOB PRIMARY KEY CHECK(length(key_hash)=32),
            intent_hash BLOB NOT NULL CHECK(length(intent_hash)=32),
            result_json TEXT NOT NULL CHECK(length(result_json)<=65536)
        ) STRICT;
        CREATE TABLE user_minecraft_names (
            principal_id TEXT PRIMARY KEY REFERENCES principals(id),
            name TEXT NOT NULL CHECK(length(name) BETWEEN 1 AND 16)
        ) STRICT;
        CREATE TABLE management_invitation_issuers (
            code_hash BLOB PRIMARY KEY REFERENCES management_pairing_codes(code_hash) ON DELETE CASCADE,
            principal_id TEXT NOT NULL REFERENCES principals(id),
            device_id TEXT NOT NULL REFERENCES devices(id)
        ) STRICT;
        DROP TRIGGER role_capabilities_builtin_no_insert;
        INSERT OR IGNORE INTO role_capabilities(role_id,capability,scope)
            SELECT id,'roles.manage','*' FROM roles WHERE id IN ('owner','administrator');
        INSERT OR IGNORE INTO role_capabilities(role_id,capability,scope)
            SELECT id,'access-sources.manage','*' FROM roles WHERE id IN ('owner','administrator');
        INSERT OR IGNORE INTO role_capabilities(role_id,capability,scope)
            SELECT id,'invitations.create','*' FROM roles WHERE id IN ('owner','administrator');
        CREATE TRIGGER role_capabilities_builtin_no_insert BEFORE INSERT ON role_capabilities
        WHEN (SELECT builtin FROM roles WHERE id = NEW.role_id) = 1
        BEGIN SELECT RAISE(ABORT, 'builtin roles are immutable'); END;
    "#)?;
    Ok(())
}
