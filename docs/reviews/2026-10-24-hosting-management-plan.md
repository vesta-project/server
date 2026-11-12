# World, software and SFTP management

Starting checkpoints: server `cc6b2a1`, launcher `16ffe1f6`. User authorized all three implementations, with Opus handling UI. No production server/world, listener, startup registration or external publication is changed by implementation/testing.

## Work and ownership

- Root Sol: World seed inspection/new-world creation, common stopped-state guards, transport composition, native client/IPC integration and final verification.
- Opus: launcher World, Software and SFTP UI, frontend models, fixtures, localization and focused checks.
- Software Sol: verified side-by-side installation, exact reviewed plans, durable asynchronous switch/rollback jobs and service tests. Contract: `docs/operations/software-management.md`.
- SFTP Sol: real optional SSH/SFTP listener and canonical policy-aware file adapter, dedicated revocable public-key credentials and bounded session/transfer tests. Contract: `docs/operations/sftp.md`.

## Decisions checked against the requirements

The existing world seed is read from bounded world metadata; the configured new-world seed is a distinct value. Seed values cross JSON/IPC as strings without JavaScript integer truncation. New-world creation requires proven stopped state, exact world-name confirmation and a current revision; existing world directories are preserved, never recursively destroyed. Stale, malformed, linked, unknown or conflicting paths refuse the operation.

Software catalog/planning/downloads are demand driven. Apply requires an owner-approved immutable plan, compatible locally selected Java and a stopped process; navigation and apply do not stop/start Minecraft. Side-by-side artifacts preserve the game directory. Actual in-memory and supervised launch state must change with the persisted definition, with durable recovery evidence and explicit rollback.

SFTP remains disabled by default, uses a separate explicit bind/port and private local host key, accepts dedicated SSH public keys on canonical Vesta principals and shares current file authorization, ignore/protected paths, trash, audit and quota checks. It grants no shell/exec/forwarding capability. Idle features add no periodic provider work. Bounded workers/transfers must preserve urgent control capacity on 1–2 core / 2–4 GB hosts.

UI uses grouped settings, existing components, capability checks, cached resources and stable intent handling. No misleading current-seed label, automatic world deletion, automatic restart, implementation notices or unfinished controls.

## Verification plan

Use inert temporary worlds, launch definitions and signed/provider fixtures. Exercise 64-bit/legacy seed parsing and malformed/compression bounds; keep/create/replay/conflict/restore cases; stopped-state/supervisor exclusion; verified install and rollback with interrupted activation; real SFTP authentication/file operations, escape/authorization/revocation/limits; frontend fixture/model/typecheck and meaningful visual states. Commit coherent progress and record unresolved platform evidence honestly.
