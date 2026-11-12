# Recovery exports

Recovery exports are plaintext by default. Use `--passphrase-file <file>` to protect an export with a passphrase read from a file; passphrases are never accepted in argv or environment variables. Protect the passphrase file, keep it separate from the export, and remove or secure it after use. The bounded local-document reader reads at most 4,098 bytes, rejecting linked paths and nonregular files. Exactly one trailing LF or CRLF is removed (if present); other bytes and whitespace are part of the passphrase. The resulting passphrase must contain 1–4,096 bytes. Set owner-only permissions or ACLs on the passphrase file yourself; the reader does not verify those permissions. Exports omit passwords, credential material, secret hashes, tokens and one-time codes.

Protected exports use Argon2id with 64 MiB memory, 3 iterations and parallelism 1, with a 16-byte random salt, then ring ChaCha20-Poly1305. The 1 MiB streaming format uses a 7-byte nonce prefix and 4-byte counter; the final-record marker is authenticated, and the header is authenticated. Keep the entire export intact; modifications, truncation or record reordering fail authentication. Unprotected exports remain version 1 JSON. The encrypted header is 48 bytes: `VESTAEX1`, a version byte, three big-endian 32-bit KDF parameters, salt, nonce prefix and big-endian segment size. Every segment has a 32-bit plaintext length and one-byte final flag, followed by ciphertext and a 16-byte tag. Its 96-bit nonce is prefix + big-endian counter + final flag; AAD is the complete header. Nonfinal segments are exactly 1 MiB, and a final authenticated segment is required. Fixed header parameters are checked before deriving the key. Readers accept at most 256 MiB of plaintext and 5,000,000 rows; decryption buffers and key derivation memory are bounded.

Stop the agent before running these offline commands. They hold the same exclusive local server leases as `serve`, and refuse with `local_server_busy_or_running` while an agent owns a lease.

```text
vesta-agent recovery-export <name> --output <file> [--passphrase-file <file>] [--install-root <root>]
vesta-agent recovery-import <name> --input <file> [--passphrase-file <file>] --confirm <server-id> [--install-root <root>]
vesta-agent backup-resolve <name> [--install-root <root>]
vesta-agent backup-resolve <name> <job-id> --confirm <job-id> [--install-root <root>]
```

The output must not already exist and must be outside both the game and state directories. Its parent directory must exist. The command reserves the destination with `create_new`, writes a temporary sibling file, flushes it, then renames it into place. Success prints `recovery_export_written tables=<n> rows=<n>`.

Containment checking is lexical; existing symlink/junction ancestors are also rejected. Alternate path aliases, later link changes and same-account races are not a security boundary. Store exports in a protected directory. They contain sensitive identity, access and audit information even without credentials.

The version 1 JSON document records the schema version, server identity, export time, and tables with named columns and row arrays. Schema 21 includes `backup_policy`. One read transaction provides a consistent snapshot. Exports fail beyond 5,000,000 rows or 256 MiB of plaintext JSON, matching the importer limits; this applies to both plaintext and encrypted exports. SQLite BLOB values are rejected, except the explicitly encoded management device public key: `management_device_keys.public_key` is exported as lowercase hexadecimal under `public_key_hex`.

Import accepts only schema 21 (`CURRENT_SCHEMA_VERSION`); older or newer exports are refused for now. The required `--confirm` must exactly match the exported server UUID. Import adopts that UUID, restores included data in one transaction, verifies the complete audit chain, then appends `recovery.import` as the protected local owner. Any rejection rolls back the identity and all rows. Import never installs, modifies, starts, or touches game or world files. Success prints `recovery_imported server_id=<id> tables=<n> rows=<n>`.

A fresh control plane has no rows in any application table except its singleton server identity, migration-seeded roles and capabilities, the exact initial `access_role_sources` singleton, and the initial `meta` watermark (`events_pruned_through=0`). The only permitted non-built-in roles are the unchanged migration defaults `public` and `player`, with their exact seeded capabilities. SQLite sequence counters must be absent or zero. In particular, principals, devices, assignments, Minecraft links, workflows, notifications, backup policy, settings, audit history, credentials, and runtime/job history must all be empty. This is not a merge into an existing control plane.

Built-in role IDs, names, ranks, builtin flags and complete capability/scope sets must match the target's protected seeds. Their `created_by` and `created_at` values are deliberately not compared or restored: the target keeps its seeded provenance and timestamps. This is the only intentional difference for included rows, apart from the additional import audit event. Custom roles and every other included row are restored exactly. The library opens and closes state privately under its runner lock; callers reopen afterwards so the database handle caches the adopted UUID.

Included tables: `server_identity`, `principals`, `user_minecraft_accounts`, `user_minecraft_names`, `user_default_roles`, `roles`, `role_capabilities`, `role_assignments`, `principal_overrides`, `access_role_sources`, `devices`, `management_device_keys`, `workflows`, `workflow_schedule`, `notifications`, `notification_state`, `pack_minimum_policy`, `settings`, `backup_policy`, and `audit_events`. Public-key digests and audit chain hashes are not hashes of secrets. No individual secret columns were found in these tables' current schema. Credentials omitted from the export must be reconfigured after import; import never recreates them.

Excluded tables: `pack_passwords`, `device_password_grants`, `bootstrap_codes`, `management_pairing_codes`, `management_invitation_issuers`, `management_owner_links`, `confirmations`, `management_permission_receipts`, `local_authority_uses`, `events`, `operations`, `operation_resources`, `resource_locks`, `file_intents`, `file_trash`, `file_uploads`, `management_file_outcomes`, `management_pack_outcomes`, `backup_jobs`, `workflow_runs`, `run_steps`, `pack_releases`, and `meta`. SQLite's internal sequence bookkeeping is not exported.

The export does not include `host.toml`, S3 endpoint/bucket configuration, S3 credential files, or the backup repository's chunks and snapshots. Optional encryption protects the exported control data, not the backup repository.

The allowlist also excludes `management_invitation_uses` (counters for excluded code hashes), `whitelist_sync_actor` (live device-bound synchronization authority), and `whitelist_sync_members` (ownership witnesses for the current game's whitelist). Import does not merge these rows from an existing target: they must be empty along with other application state. Enabling synchronization after recovery must establish its actor through the normal authorized settings flow; portable recovery does not restore game-file ownership claims.

Settings, workflow definitions, notification content and other included text can contain operator-supplied free text. There is no generic content secret detector: do not put credentials in these fields. The explicit schema allowlist excludes credential storage, but cannot identify secrets pasted into arbitrary text.

Without a job ID, `backup-resolve` lists interrupted restores as `<id> <error_code>`, or prints `no_restore_needs_recovery`. With a job ID, the confirmation must match exactly. Confirm only after inspecting and accepting the current restored files. Success prints `backup_recovery_resolved`. This marks the recovery requirement resolved; it does not restore files or start the game.

## Remote export

`POST recovery/export` is owner-only. Its JSON body may include an optional passphrase of 8–1,024 bytes. Remote exports are limited to 64 MiB; use the offline CLI for larger exports. Every remote export is audited, including whether encryption was used.
