# Recovery exports

Recovery exports are unencrypted by owner decision (2026-10-12). They omit passwords, credential material, secret hashes, tokens and one-time codes. Import is not implemented yet; this is a recovery inventory, not a directly restorable SQLite backup.

Stop the agent before either command. Both commands hold the same exclusive local server leases as `serve`, and refuse with `local_server_busy_or_running` while an agent owns a lease.

```text
vesta-agent recovery-export <name> --output <file> [--install-root <root>]
vesta-agent backup-resolve <name> [--install-root <root>]
vesta-agent backup-resolve <name> <job-id> --confirm <job-id> [--install-root <root>]
```

The output must not already exist and must be outside both the game and state directories. Its parent directory must exist. The command reserves the destination with `create_new`, writes a temporary sibling file, flushes it, then renames it into place. Success prints `recovery_export_written tables=<n> rows=<n>`.

Containment checking is lexical; existing symlink/junction ancestors are also rejected. Alternate path aliases, later link changes and same-account races are not a security boundary. Store exports in a protected directory. They contain sensitive identity, access and audit information even without credentials.

The version 1 JSON document records the schema version, server identity, export time, and tables with named columns and row arrays. One read transaction provides a consistent snapshot. Exports fail beyond 5,000,000 rows. SQLite BLOB values are rejected, except the explicitly encoded management device public key: `management_device_keys.public_key` is exported as lowercase hexadecimal under `public_key_hex`.

Included tables: `server_identity`, `principals`, `user_minecraft_accounts`, `user_minecraft_names`, `user_default_roles`, `roles`, `role_capabilities`, `role_assignments`, `principal_overrides`, `access_role_sources`, `devices`, `management_device_keys`, `workflows`, `workflow_schedule`, `notifications`, `notification_state`, `pack_minimum_policy`, `settings`, `backup_policy`, and `audit_events`. Public-key digests and audit chain hashes are not hashes of secrets. No individual secret columns were found in these tables' current schema.

Excluded tables: `pack_passwords`, `device_password_grants`, `bootstrap_codes`, `management_pairing_codes`, `management_invitation_issuers`, `management_owner_links`, `confirmations`, `management_permission_receipts`, `local_authority_uses`, `events`, `operations`, `operation_resources`, `resource_locks`, `file_intents`, `file_trash`, `file_uploads`, `management_file_outcomes`, `management_pack_outcomes`, `backup_jobs`, `workflow_runs`, `run_steps`, `pack_releases`, and `meta`. SQLite's internal sequence bookkeeping is not exported.

Settings, workflow definitions, notification content and other included text can contain operator-supplied free text. There is no generic content secret detector: do not put credentials in these fields. The explicit schema allowlist excludes credential storage, but cannot identify secrets pasted into arbitrary text.

Without a job ID, `backup-resolve` lists interrupted restores as `<id> <error_code>`, or prints `no_restore_needs_recovery`. With a job ID, the confirmation must match exactly. Confirm only after inspecting and accepting the current restored files. Success prints `backup_recovery_resolved`. This marks the recovery requirement resolved; it does not restore files or start the game.
