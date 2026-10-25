# Server software management

Forge/NeoForge research and staged implementation are documented in
[the installation plan](../architecture/FORGE_NEOFORGE_INSTALLATION_PLAN.md).
That plan does not enable either provider or claim installation certification.

Implementation contract, 2026-10-24. All routes use the existing authenticated
management session. The service independently rechecks canonical owner status and
`content.install` on every read and write, and again before activation. Anonymous,
pack-only and non-owner subjects receive `software_forbidden`.

## JSON API

Routes are relative to `/api/v3/manage`. Reject unknown JSON fields. No request
accepts executable paths, JVM arguments, filesystem paths, download URLs or scripts.

| Method / route | Request | Response |
| --- | --- | --- |
| GET `/software` | none | `{available, unavailable_reason, revision, current, previous, recovery_required, catalog:[SoftwareDescriptor]}` |
| GET `/software/versions?software=paper` | one exact software identifier | `{software, versions:[string]}`; demand-only upstream metadata |
| GET `/software/releases?software=paper&version=1.21.4` | exact software and Minecraft identifiers | `{software, version, releases:[{id,build,loader_version}]}`; metadata only |
| POST `/software/plan` | `{software, version, build?:number, loader_version?:string, experimental?:false}` | `{plan_id, confirmation_token, expires_at_ms, expected_revision, software, version, build, java, warnings, risks}` |
| POST `/software/apply` | `{plan_id, confirmation_token, idempotency_key, acknowledge_warnings, acknowledge_legacy_checksum}` | Job below; HTTP 202 for accepted work |
| GET `/software/jobs` | none | `{jobs:[Job]}` newest first, bounded to 100 |
| POST `/software/rollback/plan` | `{expected_revision}` | `{plan_id,confirmation_token,expires_at_ms,expected_revision,target,risks}` |
| POST `/software/rollback` | `{expected_revision, idempotency_key, confirm:true,plan_id,confirmation_token}` | Job below; HTTP 202 |

`current`/`previous` are nullable `{software, version, build, installation_id}`;
the original locally approved launch has null software/version and installation_id
`original`. Revision starts at zero and advances only after successful activation.
Job: `{id, kind:"apply"|"rollback", state:"queued"|"staging"|"activating"|"succeeded"|"failed"|"recovery_required", created_at_ms, updated_at_ms, error:null|string}`.
Planning tokens bind actor + device + exact resolved immutable artifact plan +
launch revision, expire in two minutes and are consumed once. Apply retries with
the same key return the existing durable job; changed requests conflict.

Release choices use the installer's existing bounded, allowlisted provider metadata.
Paper/Folia expose stable downloadable builds; Purpur exposes advertised build IDs
(planning rejects failed builds); Fabric/Quilt expose stable loader versions.
Build choices are newest first. The service uses the same single-query permit as
Minecraft-version queries and planning, and rechecks owner authorization after the
fetch. The launcher requests releases after the Minecraft-version query completes.
No catalogue request downloads artifacts or changes launch state. Plans also return
the resolved `loader_version` so the review shows the exact selected loader.
Rollback explicitly selects the durable previous launch, preserves both software
installations, and never downgrades/reverts world data. It requires an exact
revision and confirmation; warn that an upgraded world may require backup restore.

Errors use the existing transport envelope with fixed `software_*` codes:
`forbidden`, `disabled`, `invalid_request`, `busy`, `plan_expired`,
`confirmation_invalid`, `revision_conflict`, `idempotency_conflict`,
`previous_unavailable`, `recovery_required`, `state_unavailable`,
`runtime_unavailable`, plus the installer's redacted `server_install.*` failures.

## Runtime and persistence integration

Implemented automatic providers: Vanilla, Paper, Folia, Purpur, Fabric, Quilt,
and the shared modern Forge/NeoForge processor adapter. The latter is implemented
but awaits real installation/platform testing. Legacy Forge 1.7–1.16 profiles now
have a separate Java-8 adapter; prerelease-only loader versions remain refused.
See the [legacy checkpoint](../reviews/2026-10-25-legacy-forge.md) and the
[implementation checkpoint](../reviews/2026-10-25-forge-installation.md).
Provider resolution,
allowlisted HTTPS downloads, pinned checksums, selected local Java probing and
disk reservations reuse `vesta-server-install`.

The protected local configuration supplies the installer root, Java, game root,
and launch document; these are never remote request fields. Downloads and hashing
run on one dedicated demand-driven worker, not the urgent management dispatcher.
No idle timer or provider polling is installed. At most one switch runs at once.

The owner stops Minecraft separately. Applying never stops, starts or restarts it.
Before activation the runtime adapter obtains the same start exclusion used by
world restore/backups and a supervisor stopped-game maintenance hold when present.
It must positively establish stopped, non-draining, no pending restart, and refuse
unresolved process ownership. It atomically replaces the protected launch document
and the stopped process owner's launch specification while the hold is retained.
The new launch uses the existing game directory; world/config/EULA/mod/plugin bytes
are untouched. Side-by-side artifacts live outside that directory. Activation is
explicitly approved by apply. A later ordinary Start is separately authorized.

Before replacing a launch, persist both the exact previous and candidate launch and
an activating journal record. An interrupted activation becomes recovery_required
and blocks starts on reopen. The operator can use explicit rollback to the stored
previous launch; never blindly replay an ambiguous activation. Interrupted staging
fails without touching the live launch. Durable idempotency survives agent restart.
The runtime adapter must restore the selected active launch on host startup before
automation can start. It must not merely edit TOML while retaining a stale in-memory
or separate-supervisor launch. Remote software management reports unavailable
until this adapter is attached.

Cross-family/version compatibility is advisory, never a promise: mods/plugins,
Folia threading, dimension layouts and world downgrades can be incompatible. The
plan always requests backup review and records these warnings. No world backup is
silently manufactured or declared successful by this service.
