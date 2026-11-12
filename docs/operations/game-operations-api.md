# Closed Rust game operations

Date: **2026-10-09**. Status: **implemented and Windows-tested bounded composition; scoped Sol rereview completed with correction; final whole-rebuild review unfinished**.

This is a Rust-native trusted host composition over the existing state, identity and
owned game-process components. It supersedes the staged-only execution scope in
[RUST_OPERATIONS_KERNEL_PLAN.md](../archive/RUST_OPERATIONS_KERNEL_PLAN.md) for this closed
action inventory, not for arbitrary handlers or the complete Java wrapper rebuild.
Executed results belong to [the integration record](../archive/RUST_INTEGRATED_MIGRATION_PASS.md),
not to this API description.

## Trusted application boundary

`Operations::new(StateDatabase)` retains current-policy task observations and cancellation.
`Operations::submit_game(&Subject, GameCommand, Option<&str>)` admits only:

- `GameCommand::Start`: canonical `{}`, `lifecycle.start`, fixed `game.generation` lock.
- `GameCommand::Stop { expected_generation }`: exact opaque generation, `lifecycle.stop`,
  the same fixed lock, idempotent action declaration.
- `GameCommand::Restart { expected_generation }`: current lifecycle.restart authority.
- `GameCommand::Console { expected_generation, line }`: separately authorized console.write.
- `GameCommand::Moderate { expected_generation, action }`: closed validated typed actions,
  players.moderate except OP/de-OP (players.operator); OP requires exact confirmation.
  Reasons are trimmed, at most 256 characters and 512 UTF-8 bytes; the native
  1024-byte guarded-line ceiling stays unchanged. Running-only, same guarded stdin
  write. Success proves delivery, not game application.
  History and temporary expiry are durable schema-v22 records. See
  [the implemented management contract](management-api-contract.md#typed-moderation-f34-implemented-2026-10-13).
- `GameCommand::ForceKill { expected_generation }`: exact generation, `lifecycle.force_kill`,
  the same fixed lock, explicit confirmation, non-idempotent action declaration.

Lifecycle/moderation clients cannot supply capabilities, locksets, safety flags, source revisions, stdin commands,
executables, paths, URLs or launch arguments. Generation hex is a strict lowercase 32-digit
lookup encoding, not a PID, attachment mechanism or authorization proof. Subjects are trusted
application lookup keys, **not authenticated transport evidence**. Authenticated management
routes live in the transport composition, not in this trusted application API. Passwords, Minecraft receipts, OP, loopback and possession
of a process/connector do not create administration.

`OperationRuntime::open(operations, LaunchSpec)` acquires this database directory's actual
runner lock and a runtime database lease, reconciles durable work, and owns one autonomous
polling driver. The literal validated launch spec comes only from trusted host composition.
`open_with_stop_timeout` accepts a bounded trusted host duration, not an operation field.
No caller-driven tick, public handler registry, injected policy or revision provider exists.
`game_snapshot` and `status` expose bounded ownership facts, not readiness or console secrets.

## Effect and lifetime boundaries

Admission is not a future execution permit. Exact persisted input/hash/binding, claim token,
current principal/device policy, cancellation, named locks and accepted receipt are checked at
claim/run/effect boundaries. Native spawn/kill preflight occurs before the fresh effect gate.
The effect marker and audit/event commit precede the native call; SQLite cannot atomically
commit an external process effect. Uncertain started work becomes nonterminal
`recovery_required`, retains its locks and is never blindly replayed.

Guarded fixed `stop\n` uses the **existing** bounded queue-eight stdin writer. Queue acceptance
is not delivery. The owned callback performs a fresh writer transaction immediately before
native write, and a typed acknowledgement represents actual callback/native return. Pending
callbacks retain exact operation/core/database/OS ownership even after a wait expires.

Only after a successful actual stop-write acknowledgement does the bounded game-exit timer
begin. A still-live game that ignores the request may produce task failure `game_stop_timeout`.
That releases only the completed operation's lock: the exact child, generation and all native
I/O remain owned. A later currently-authorized, confirmed force-kill can target that generation.
An unanswered write, uncertain write, or child exited with unjoined inherited pipes must not
use this timeout as permission to discard work. Native observation failure is sticky: it stops
new dispatch, makes the published confirmation revision unavailable, and disables ordinary stop
deadlines even if a later write acknowledgement or poll succeeds. Exact claims remain owned until
actual child exit and every I/O join, then record uncertainty/recovery rather than invented success
or timeout. Start/replacement remains refused while any generation is live, stopping or draining.

Known returned native outcomes are private factual bookkeeping, not new action authority.
Revocation cannot retroactively undo an observed effect or prevent recording a clean pre-effect
refusal. Finish retains exact canonical ownership checks, resolves current policy, records fixed
current-denial metadata, and rolls back on infrastructure/audit errors. No result may invent
success before an effect marker, cancellation after a started effect, or recovery resolution.

`request_shutdown` refuses new claims/delayed gates synchronously. `shutdown(timeout)` is
observation-only; timeout retains the same driver join handle, claims, child and I/O. It sends
no automatic stop/kill/restart. Sticky native observation failure raised during dispatch is
preserved in the driver's typed join result even if task bookkeeping and later native polls
succeed; it cannot silently become a successful first shutdown join. Runtime Drop requests
shutdown and joins indefinitely if needed;
it is intentionally not a bounded shutdown or independent supervisor/crash-continuity claim.
The database's explicit close returns `state_runtime_busy` while a runtime lease lives and leaves
transactions available for real outcome writes. The lease is lifecycle retention, not authority.

## Confirmations

Only the concrete runtime issues and consumes confirmations against its privately published
actual-generation view; database callbacks never acquire the mutable process owner.

- `issue_confirmation(&Subject, &OperationId, Duration)` requires exact initiating actor/device,
  awaiting state, canonical policy and current generation. TTL is positive and at most 15 minutes.
- `confirm(&Subject, &OperationId, ConfirmationToken)` takes an owned, bounded, zeroizing,
  redacted, non-Clone token. Issued tokens contain 256 bits of OS randomness; only their hash persists.
- One admitted writer burns the token before normal binding/policy/expiry/revision checks.
  Intentional refusal commits burn and denial audit together through a private typed decision.
  Acceptance commits receipt, queued state, audit and lifecycle event together. Unexpected
  infrastructure, mutation or audit failure rolls the entire transaction back.
- Schema 6 adds nullable `accepted_at`, with **no backfill**. A consumed/refused/legacy token is
  not accepted proof. Exact server/operation/actor/device/action/target/payload/revision,
  supersession and valid acceptance timestamps are required. Token TTL bounds the opportunity
  to accept, not a legitimate accepted queued operation's eventual execution time.

## Sanitized historical replay

`replay_open`, `replay_page` and `replay_checkpoint` check current `tasks.view` within each
read snapshot, including empty pages. Cursors are opaque in-process values bound to exact
server/principal/device. Continuation preserves the captured horizon; explicit reopen captures
a new horizon. Scan work is bounded independently of returned events (500 rows / 100 events).
Pruning gaps require resynchronization; a head checkpoint alone is not atomic state-plus-journal
synchronization.

Projection returns only historical event cursor/kind/time and validated operation ID/state/numeric
progress. It never returns raw input/result/progress text, actor identities or binding headers,
or fabricates history from a current summary. Task viewing does not require mutation authority,
retained input, or the initiating actor's current grants. Unknown/unavailable rows advance the
scan; recognized malformed eligible payloads fail with fixed redacted errors.

## Explicit unfinished scope

No durable restart, recovery-resolution command, remote authenticated management, independent
supervisor/protected IPC, process-tree containment, readiness, production OS bootstrap/device
flow, installer activation/update/world restore or full host executable is supplied. Retained
Java behavior accounting, the complete original requirements, platform/MSRV/release gates and
final independent whole-rebuild review remain separate and unfinished. Only owned inert native
children and local SQLite/filesystem fixtures may be used for this candidate's Windows tests.

### Ban plugins are not supported yet

Vesta currently offers only its own vanilla-command moderation. Ban plugins such as
LiteBans, LibertyBans, AdvancedBan and BanManager often replace /ban, /pardon and
/kick and store bans in their own database instead of banned-players.json. With
such a plugin, Vesta commands may be handled by the plugin; the vanilla ban/OP/
whitelist read views and Vesta history do not reflect plugin ban databases. History
covers only actions issued through Vesta: bans made in-game are not recorded.
A temporary ban whose current vanilla file entry cannot be confirmed is marked
superseded and produces an inbox warning, not a silent permanent ban. Review that
ban manually. Plugin and connector integration is future work; there are no plugin
adapters or plugin settings in this feature.
