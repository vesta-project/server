# Next Rust increment: durable-operations kernel

Date: **2026-10-08**. Status: **scoped design only; no implementation or execution evidence**.

This is the next bounded slice of the approved [Rust-native migration](RUST_WRAPPER_MIGRATION_PLAN.md),
after [the Minecraft checkpoint](RUST_MINECRAFT_IDENTITY_CHECKPOINT.md). It preserves the original
370 requirements; it is not a new product specification, complete operations phase or Java API port.
GPT-6.1 Sol inspected the retained Java operation/binding/authority/runner/confirmation/replay
references and current Rust schema/policy/transaction/runner-lock APIs read-only. Parent retains
architecture/integration/verification; Luna may translate bounded test families after the API settles.

## Selected useful slice

Build a cohesive `vesta-control::operations` kernel over existing state foundations:

- bounded validated canonical input and a private server-derived operation plan;
- exact persisted idempotency after current-policy resolution;
- sanitized operation summaries and atomic lifecycle audit/event records;
- private OS-lock-owned claiming with all-or-none sorted resource locks and conflicting FIFO fairness;
- atomic cancellation of unclaimed work; claimed cancellation only requests cancellation;
- conservative startup reconciliation and terminal-input retention.

Do **not** add production execution handlers, background workers, external-effect dispatch,
confirmation consumption, event replay or transport wiring in this slice. No fake successful/no-op
handler, empty binary or unavailable route may masquerade as production execution. Kernel test
bindings must be private `cfg(test)` fixtures. The closed reviewed production binding boundary
will be populated by actual domain implementations, not caller-provided capabilities or locks.

## Reuse and ownership

Proposed source organization (not one file per Java class):

- `operations/mod.rs`: narrow trusted in-process facade;
- `operations/domain.rs`: validated IDs, lifecycle, redacted summaries and dedicated typed errors;
- `operations/input.rs`: bounded canonical representation/hash and private derived plan;
- `operations/repository.rs`: private SQL borrowing the caller's transaction, never opening one;
- `operations/queue.rs`: owner session, claiming, cancellation and reconciliation.

Schema version 5 already has `operations`, `operation_resources`, `resource_locks` and
`confirmations`; v2 added `input_json`, `input_status` and `owner_token`. Idempotency uniqueness is
`(server_id, actor_principal_id, idempotency_key)`. Do not invent a migration unless inspection of
the finalized Rust domain requires one. The existing protected database directory owns the
`RunnerLock`; obtain it through `db.directory()`, never a caller-selected unrelated path.

Use `StateDatabase::write_with` / `TransactionError<E>` and caller-connection audit/event appends.
Add a narrow **crate-private transaction-taking policy resolver** reusing the identity repository.
Do not duplicate policy SQL or nest `IdentityService::current_policy()` inside a writer. Sample
transaction time after writer admission. Parent owns this identity seam, module wiring, shared
manifests/migrations/accounting/builds. A Sol implementer may own the new operations sources and
focused tests; Luna must not design production authority or weaken assertions.

`Subject` remains trusted application lookup data, not authenticated device evidence. Public
trusted-library composition is not a network authorization boundary. Source labels, loopback,
Minecraft receipts/password matches and test fixtures confer no local/admin authority.

## Safety contracts to settle before implementation

1. **Input and bindings.** Limit raw/canonical UTF-8 bytes before parsing/allocation; 64 KiB is
   the proposed conservative first bound, not a new normative product constant. Reject unknown
   fields, duplicates, NUL and unsupported forms. Derive operation type/target, capability/resource
   requirements, sorted locks and safety declarations server-side. Hash exact persisted canonical
   bytes and require a stable round trip, without Gson/JDK emulation.
2. **Bound persisted reads.** SQLite `length(input_json)` is not a byte/allocation bound and
   undercounts NUL-bearing text. Guard SQL projection before materialization, verify canonical
   input/hash/status and bound resource-row reads with a sentinel extra row. Missing/purged legacy
   input cannot be reconstructed from a hash. Corruption must not widen authority or replay safety.
3. **Idempotency.** Resolve current policy before returning even an existing operation. Match
   hash/type/target/device/server-selected source, exact sorted locks and both safety flags.
   Principal/server bind the lookup key; changed device or source is a conflict, not an authorized
   retry. Caller-selected lockset/capability/revision/local authority must not enter remote commands.
4. **Fair claims.** One immediate transaction inspects queued rows in `seq` order. Held locks
   start the unavailable set; each earlier blocked operation reserves its entire requested lockset
   against later conflicting work. Independent work may progress. Lock inserts, private owner
   token, validating transition, audit and event commit/roll back together. Exact lock-key equality
   is not hierarchical filesystem confinement; future bindings must derive conservative conflicts.
5. **Private ownership.** Cryptographic owner tokens have no serialization/unrestricted Debug or
   NULL-owner mutation bypass. Owner predicates belong in lifecycle updates. Infrastructure/audit
   failure stops dispatch rather than becoming a handler outcome. A dropped owner session never
   fabricates completion/cancellation; outstanding rows remain for reconciliation. With no workers,
   this is **not** evidence of uncooperative-worker shutdown.
6. **Recovery.** `recovery_required` is nonterminal: no finished timestamp, input purge, lock
   release or blind replay. A stored idempotent bit alone cannot authorize replay of an unknown or
   unverifiable binding/effect. Contradictory or missing recovery-lock ownership must stop dispatch,
   not free conflicts. Unknown started effects remain blocked for explicit later recovery.
7. **Cancellation.** Unclaimed work can cancel atomically. Claimed work only records a cancellation
   request/state; it must retain locks/token/ownership until the actual executing owner finishes.
8. **Future effects.** Admission does not authorize an eventual effect. Later run/effect paths must
   reload persisted binding/current policy and owner/cancel checks in the effect transaction with
   a durable effect marker. SQLite cannot make external files/process/network effects atomic; the
   marker commits before the effect, and uncertainty remains explicit.

## Acceptance and fault families

Translate useful behavior, not Java method surfaces, with real SQLite/OS lock fixtures:

- concurrent exact retries yield one row; altered binding or current revocation refuses retrieval;
- unstable canonical semantics, corrupt hash, unavailable input, oversized/NUL text and resource overflow;
- multi-resource oldest-blocked fairness, transitive conflicts and independent queue progress;
- second-lock insertion, audit/event append and COMMIT faults leave no partial state/locks/claim;
- foreign/stale owner token rejects without disclosure; OS lock contention refuses before reconciliation;
- queued cancellation versus claimed cancellation retaining resource locks;
- safe interrupted versus uncertain/unbound started work after reopen; recovery rows never purge/replay;
- policy changes during writer admission; no nested policy snapshots or caller-retained grants.

The private API and binding limits must settle before fixture translation. Neither this list nor
the read-only design review is a passed test or parity mapping. Existing Java sources/builds stay
available. No operations inventory row or whole requirement is promoted by this document.

## Deferred composition and prohibitions

Actual worker supervision must retain join handles, OS lock, database and operation ownership
through genuine exit, including startup/shutdown overruns. A timeout, aborted task, dropped future
or dropped join handle is not termination. Real worker lifetime tests belong to that later increment.

Confirmation-required production bindings remain disabled until a real gateway exists. It must
server-derive action/target/hash/revision, bind principal/device/server/operation/expiry, persist only
a token hash and atomically single-use consume. A refused presented live token burn and audit must
commit together; a separate post-rollback refusal audit cannot establish the burn invariant.

Authorized replay must later check exact current `tasks.view` in the read snapshot, use fixed-horizon
bounded scans/cursors and strict sanitized projections with retention resync. Raw EventLog reads are
not remote APIs. Recovery resolution, priorities/coalescing, game/files/installer bindings,
authenticated transport/device composition and real OS local authority remain deferred.

Windows-only execution is the current owner direction; other platforms/architectures/MSRV and
mandatory final whole-rebuild independent Sol review remain unverified. No real provider/Minecraft,
EULA acceptance, world/demo erasure, service/firewall/router/public exposure, signing/security-setting
changes, automatic world restore, push/PR/publication or protected launcher edits are authorized.

Later owner authorization, 2026-10-08: make **local unsigned checkpoint commits** as verified chunks
are ready. Use only a per-command signing override; do not change repository/global signing settings,
request a PIN, silently bypass a failing hook or push. This does not authorize publication or promote
migration/product completion.
