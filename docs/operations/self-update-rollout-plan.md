# Coordinated Vesta updates

Design checkpoint, 2026-11-10 Australia/Sydney. Activation is not implemented.

## Scope and decisions

Keep Vesta binaries, Minecraft software/modpack changes and launcher updates as
separate operations. Release maps to signed `stable`; Nightly is explicit opt-in.
An installation's OS-local administrator owns update policy. A remote per-server
Owner must not gain rights over every installation consumer on the machine.
No registry is inferred by enumerating arbitrary processes/directories.

Current code implements pinned signed trust, expiring sequence-bound metadata,
side-by-side resumable staging, exact saved manifest proof, fresh offline staged
verification, an advisory rollout preview, and opt-in background startup discovery.
The public `vesta update` command invokes the same implementation as setup.
No check/stage/verify/configure operation replaces binaries or restarts services.
`vesta-agent` remains an internal compatibility alias for current registrations
and signed packages. Client onboarding remains a separate future task.

## Required activation sequence

1. Establish an exact installation consumer registry and installation authority.
   Candidate IPC/schema compatibility must be signed/versioned evidence, never
   inferred from similar version strings or remote caller assertions.
2. Recheck trust, expiry, target, metadata/manifest signatures and opened binary
   hashes at execution. A staged flag or serialized verification observation is
   not execution authority. Retain the existing anti-rollback floor.
3. Fence new mutations and resolve pending install/file/restore journals, restart
   intents and unknown generations. Do not stop workers by killing processes.
4. Use versioned payload directories and a stable dispatcher/helper. Do not
   overwrite a running Windows executable or call four sequential replacements
   atomic. Service-registration changes and active selection need a durable journal.
5. Trial the candidate without admitting normal mutations. Validate exact running
   release, server/TLS identity, protected supervisor binding, state compatibility
   and job reconciliation. Process existence alone is not health. Do not launch
   Minecraft merely to test the updater.
6. Commit only the promised consumer scope. Compatible separate supervisors may
   remain while their game runs; mark their binary update pending until stopped.
   In-process ownership or incompatible/unproven IPC requires stopped maintenance.
   Unknown state blocks; pending writes/restarts defer. Retain crash-loop evidence.
7. Recovery may select only the exact prior committed healthy payload under the
   recorded intent. It must not lower the accepted download floor or restore a
   stale database that revives revoked access. Incompatible committed schema writes
   require forward recovery or a specifically approved stopped migration plan.

The current pure rollout preview encodes conservative outcomes for these cases
but executes none of them. Empty or mixed consumer observations cannot claim
activation support. A real executor must refresh every authority/identity fact.

## Discovery and automation follow-ups

The current operator-selected directory can report its own signed release; it is
not a channel catalog. Design signed discovery with immutable artifact identities
and fresh bounded attestations for long-lived stable releases. Current packaging's
short expiry cannot certify an unchanged stable release indefinitely. Preserve
per-channel history when changing channels; do not delete trust to switch. Key
rotation/revocation is separate trust work, not downloading a replacement key.

Startup discovery is opt-in and metadata-only. A persisted pre-I/O admission spaces
attempts across restarts; failure leaves healthy installed services usable. There
is no periodic poller or automatic application default. A future coordinator can
accept CLI, startup-policy and automation submissions with the same durable intent
and maintenance-window semantics. Losing a launcher connection must not duplicate
or abandon an accepted rollout.

The CLI may later offer a separate Minecraft/modpack update check after a Vesta
update, but each server still needs its own version/loader/world/confirmation
contract. No blanket game upgrade, world deletion or EULA acceptance is implied.
Integrate update notices with the existing automation inbox only after exact
installation-to-server scope and candidate deduplication exist. See
[performance and alerts](performance-capture.md).

## Next acceptance gates

Use temporary owned roots and fake runtime hooks first: helper replacement and
Windows image locks; Linux dispatch/registrations; failure before/after journal and
selection effects; mutation-fenced health trial; compatible supervisor handoff;
incompatible schema refusal; mixed consumers; same-intent retry/lost response;
offline discovery; revoked authority; no restart loop. Real Minecraft continuity,
physical power-loss durability and installation migration require separate evidence.

Final numeric hard-resource budgets remain unapproved proposals. This design does
not activate them, relax provider transport policy or introduce new signing keys.
