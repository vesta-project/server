# Server modpack readiness — 2026-10-23

Review scope: current Rust server and rewritten launcher server integration.
Checked against requirements sections “Live server files and hosted modpack
publication”, “Pack revisions and update policy”, and implementation-plan sections
8–9. The roadmap still classifies the relevant features F19–F24 as partial or
prototype. Some individual roadmap descriptions are older than the code: content
installation and pack publishing now have management HTTP routes.

## Implemented foundations

- Configured pack sources: selected-directory builds, local archive hosting,
  including `.mrpack`, and Modrinth/CurseForge catalogue references. Platform
  references describe a client pack; they do not fetch/install it on the server.
  See `config::PackSource` and `Host::prepare_pack`.
- VestaIndex projection, provider-reference/hash primitives, bounded pack builds,
  content-addressed artifacts, durable monotonic release records, expected-revision
  conflict checks, idempotent remote publication, minimum-revision policy, and
  pack-access-gated downloads. See `crates/vesta-control/src/packs/` and
  `crates/vesta-protocol/src/pack/`.
- Agent HTTP `pack/publish` and `pack/minimum`, with native launcher client and
  Tauri command wiring. Rewritten launcher Overview actually renders pack
  revision/minimum plus Publish and Require Update controls. This is more than
  fixture-only UI; no live publication was performed during this review.
- Individual mod downloads from Modrinth/CurseForge resolve identifiers server-side,
  restrict download hosts and redirects, verify expected hashes, and use bounded,
  journaled file writes. The agent exposes `content/install`. This is not a
  complete modpack installer or dependency resolver.

## Incomplete user workflows

- No dedicated launcher Pack tab/editor, complete draft manifest CRUD, provider
  selection and provenance editing, overrides/import preview, or release-diff flow.
  The CLI scoped help also exposes no pack management commands.
- No complete install/update of an entire server pack: archive/dependency
  resolution, client/server environment filtering, loader/Java compatibility,
  missing/manual provider downloads, coordinated apply and rollback need an
  integrated workflow. Existing client-instance modpack installation is separate
  from installing Minecraft server contents.
- Configurable sources and build primitives do not fulfill the required managed
  draft/overrides workspace lifecycle. The current publish HTTP adapter supplies
  an empty `SourceRegistry`; there is no integrated draft provider-reference store.
- The content installer currently finishes via the ordinary file-write path.
  Managed JAR staging until actual game exit on an approved restart is incomplete;
  it must be implemented before calling this a safe running-server updater.
- Complete hosted-pack consumption in the rewritten server integration is absent:
  creating/updating a linked client instance, signature verification bound to the
  paired server identity, rollback rejection, compatibility, and per-revision
  symlink consent still need implementation and end-to-end validation. Pack
  hashes and monotonic hosting records alone do not establish signed trust.
- Scheduled publication, the full optional/required update experience, stale
  server-to-pack suggestions and revision-specific link support remain gaps.

## Suggested implementation order

1. Define the persisted dedicated workspace and immutable provider references,
   using the existing VestaIndex contract. Keep it outside live server files.
2. Add bounded, authorized draft/edit/import/diff/build APIs and scoped pack CLI
   commands, then the launcher Pack editor using the same APIs.
3. Complete server-identity signatures and a tested linked-client install/update
   flow; verify failure, unavailable sources, rollback and optional entries.
4. Add whole-server pack installation and managed updates with reviewed plans,
   disk/RAM/download budgets, approved process exit, rollback, and optional safety
   backups. No implicit restart or automatic server-to-client copying.

These are readiness findings and a proposed order, not newly completed features.
No external provider network tests or destructive installs were performed.
