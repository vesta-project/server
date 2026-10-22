# Server / launcher integration review — 2026-10-22

## Scope and checkpoints

Only `server-connections-mvp` (`feat/launcher-compat`) and the server integration in
`launcher` (`feat/frontend-rewrite`) are in scope. Shared launcher auth, transport,
installation, persistence and Tauri infrastructure are in scope where integration
depends on them. Other sibling repositories are excluded.

Local baseline tag in both repositories: `review-baseline-2026-10-22`.

- Server baseline: `752fd17cff14e4286ff935dfc01f8fcb56ece033`.
- Launcher baseline: `24fb0eeb1f3075eb183b3651a5686b3f116608bc`.
- The launcher checkpoint fixes same-server snapshot updates repeatedly loading
  settings, resetting unsaved drafts/errors, and churning topic subscriptions.
  The regression reproduces 61 loads for 60 updates before the fix, one load after.
- Baseline validation: TypeScript passes; 39 focused frontend tests pass. Oxlint
  runs under Bun and exits successfully with existing warnings. The default lint
  script's installed Node 22.14 cannot load its TypeScript config.

## Working method

The parent verifies findings, owns regression tests, prioritizes changes and commits.
Luna gets narrow source scans with separate finding files, no repeated full-repo
onboarding. Findings need a concrete trigger, consequence, source location and a
credible reproduction. Hypotheses stay labeled; test success does not establish
untested platform/network behavior. Each implementation batch records validation.

No production deployment, OS trust-store edits, public exposure, destructive
cleanup or unrelated launcher redesign is implied. Preserve current user data.
Use scratch fixtures for fault injection; coordinate any necessary live restart.

## Review coverage

1. TLS trust, the eMudhra incident, auth/identity boundaries, redirects, secrets,
   downloadable artifacts and update trust.
2. Server process ownership/recovery; sleep/offline/wake; backups and storage;
   SQLite writer contention, lock duration, blocking work and idle overhead.
3. Request admission, backoff, subscriptions, disconnects, cancellation,
   idempotency, slow/unreliable networks, memory/disk/CPU budgets.
4. Launcher reactive state, draft retention, error recovery, IPC payloads,
   multi-window behavior and server-management experience.
5. Measurable adaptive behavior: reduce optional work under pressure, protect
   lifecycle controls, preserve correctness rather than silently dropping effects.

## Initial evidence

### R01 — Repeated settings loads on status snapshots (fixed at baseline)

Confirmed with installed Solid 2 diagnostics. A raw ID accessor passed to an
effect still reruns when its parent snapshot changes. A memoized identity gates
policy loads and subscriptions, preserving drafts and visible failures. The same
pattern was fixed in Settings, Sleep/MOTD, Access, profile, logs, trash and topics.

### R02 — eMudhra trust exposure (investigation)

Primary incident report: https://bugzilla.mozilla.org/show_bug.cgi?id=2083449

Upstream verifier patch: https://github.com/rustls/webpki/releases/tag/v%2F0.103.16

Upstream root update: https://github.com/rustls/webpki-roots/releases/tag/v%2F1.0.10

The incident includes compromised intermediate keys and cross-signs. OS trust
updates do not establish the safety of applications using independent roots or
verifiers. Inventory actual resolved dependency graphs and client construction
sites before choosing a mitigation. Pinned Vesta agent identity and public-CA
provider/auth/download connections need separate analysis.

## Plan status

Checkpoint complete. Review scans and trust inventory in progress. Do not present
a final implementation plan until the evidence is consolidated and reviewed a
second time. Subsequent sections will record that review and implementation.

## Resume notes

The development agent is the updated staged binary at
`%LOCALAPPDATA%/Temp/vesta-dev-server/bin/vesta-agent.exe`. It was restarted cleanly
after a graceful Minecraft stop. Real authenticated management reads returned 200
in 35–73 ms at that point. Do not assume those timings cover sleep transitions or
slow hardware. The current frontend checkpoint is loaded by the running dev server.

Next: narrow Luna scans; dependency trust inventory; technical primary-source
research; validate and rank findings; write/review plan; implement in small commits.
