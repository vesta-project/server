# Local operator design

## Inventory and authority

| Owner need | Classification | Local boundary |
| --- | --- | --- |
| Status | Read-only | Redacted identity, configured modes; not live process health. |
| Pack status/revision | Read-only | Configured and latest hosted revision/minimum; artifact presence only. |
| Publish pack | Mutating | Canonical local owner authority; explicit publication and audit. |
| Adopt/install server | Mutating | Canonical local owner authority; validated plan, confirmation, durable intent and audit. |
| Check config | Read-only | Validate without preparing state or starting runtime. |
| Pending-intent/recovery view | Read-only | Counts now; any future listing must redact targets and payloads. Reconciliation/retry is mutating. |
| Audit tail | Read-only | Future bounded, redacted projection; never raw metadata, principals, device IDs or targets. |

Read-only inspection needs no administrative authority beyond OS permission to read the redacted state. It must not manufacture authority or initialize/migrate state. Publication, installation, adoption, owner setup, recovery actions and device management are design only, not new commands in this change.

## Recommended authority model (not implemented)

Implement genuine, explicit one-time owner bootstrap: verify the caller's OS identity using a reviewed native adapter (Windows token SID / Unix effective uid), bind it to the canonical server policy, and restrict the state-directory ACLs/permissions to the required identities. Bootstrap must refuse an already initialized owner policy and must not silently infer an owner from files. Every subsequent mutating command must verify the OS identity against current canonical policy, recheck ACL/policy integrity, resolve capability/confirmation requirements, and audit admission and outcome. Owner replacement/recovery needs a separately designed, explicit, audited procedure; no reset-on-missing-file shortcut.

Mere file ownership is insufficient: files can be copied, created by the agent/service, or replaced by a same-account process. Reading a config, possessing a connector/bearer, owning a process, OP, loopback/LAN access, or Minecraft receipts never grants administration. ACLs protect policy storage; they do not themselves appoint an owner. Microsoft/Minecraft identity remains the normal player flow; pack passwords are pack-access only.

Alternatives include a separately enrolled administrative credential/device with proof of possession and canonical policy, or a privileged OS-mediated management broker with explicit caller authentication. Neither is implemented; an arbitrary bearer or password is not a substitute for enrollment and verification. Production OS-local bootstrap/device-management authentication remains unavailable until genuinely implemented.

### Open owner decisions

Agent/game separation into different OS accounts is P3, not resolved here. Options include running both under one owner account (no same-account isolation), a dedicated agent account with separate game account, or an OS-mediated broker separating operator and service identities. Decide who may read state and which service identities need access without treating those identities as owners. Separately choose administration by enrolled OS identity, enrolled administrative credential/device, or a future broker, and define recovery/rotation. No option is selected on the owner's behalf.

## Implemented CLI boundary

`vesta-agent status <path>` and existing `check-config <path>` are read-only. Strict positional parsing rejects unknown commands, flag-like config paths, missing arguments and extra arguments. `serve` remains the existing runtime command, not an administrative grant.

Status reads existing current-schema state only, using an immutable read-only SQLite connection. It refuses any WAL, SHM or rollback-journal companion before and after inspection: this initial implementation is for quiescent/offline state, not a live status endpoint. Stop the agent normally before inspection; status does not stop it. No state preparation, ACL tightening, migrations, runner lock, recovery, artifact directory creation, audit append, listener or game execution occurs. Missing state and failures yield fixed redacted errors. No configured paths, secrets, SQL diagnostics, display names, hashes or raw audit payloads are printed. Artifact presence means a plain file of the recorded size, not digest verification or a download grant; artifact and database observations are not one atomic filesystem snapshot. Filesystem checks are point-in-time same-account hygiene, not hostile-race confinement; a caller must keep state quiescent throughout inspection. A future live read-only snapshot API needs its own design rather than ignoring WAL.

Status reports a redacted identity marker, configured pack-access/listener modes, configured and latest hosted pack revision/minimum, latest artifact presence, file-intent pending/recovery-required counts, operation pending/recovery-required counts, and last audit sequence. A detailed audit-tail command is deferred. No launcher, service, firewall, Minecraft or Java changes are included.

## Owner decision (2026-10-10)

For now, local CLI commands rely on the operating-system account, as the owner decided. The owner-bootstrap recommendation above remains the design for remote management and any stronger local authority. See section 21 of docs/SERVER_SOFTWARE_IMPLEMENTATION_PLAN.md.
