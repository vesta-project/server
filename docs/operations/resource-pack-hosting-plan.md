# Server resource-pack hosting plan

Status: researched and planned, not implemented. The accompanying
[research report](../reviews/2026-10-27-resource-pack-hosting-research.md) records
primary sources and the limits of the merging evidence.

## Product rules

- Plugins install into the server's plugin directory. Datapacks install into the
  configured main world's datapacks directory. These never offer a client-pack
  destination in Browse.
- Mods can target server resources, the client modpack, or both when compatible.
  A saved client-pack draft qualifies before publication; an untouched draft does
  not. A sole applicable destination skips the destination selection screen.
- Resource packs can belong to a client modpack or to a separate server-hosted
  resource-pack workspace. Both workspaces retain their own inputs and releases.
  Browse must not offer hosting until that service and its origin are configured.
- Server-hosted packs work for vanilla and Bukkit-derived servers without requiring
  a Vesta client modpack. No implicit copying between the two workspaces.
- Preserve the existing design. Opus owns the management UI work; backend contracts
  and tested behavior should be ready before exposing new destinations.

## Phase 1: one hosted pack

Create a service-owned resource-pack draft containing the target Minecraft version,
pinned provider/upload input, optional/required policy, prompt, stable logical pack
UUID and expected revision. Reuse authoritative provider resolution and bounded
upload/download mechanisms; clients supply identifiers, never trusted download URLs.
Validate the archive before accepting it. Treat filters/overlays and format ranges
as versioned metadata, even for a single input.

Build a release to protected disk staging. Validate the finished ZIP, calculate
SHA-1 for Minecraft and SHA-256 for Vesta, then atomically publish an immutable
artifact and its release record. Existing releases remain intact on failure.
Use hash-addressed download URLs, with stable bytes for each URL. Installing inputs
edits the draft; review/build/apply controls publication.

Vanilla clients need a direct download without Vesta login or custom headers.
Require an explicit public HTTPS origin with a certificate ordinary clients trust.
The management connection's pinned/self-signed identity does not establish that
trust. An operator-configured reverse proxy/static origin can serve the exact
artifact route; external object storage is a future adapter. Do not expose other
management or file routes when enabling this download route.

Review then apply the URL, SHA-1, UUID and optional prompt/required settings together,
using the existing server.properties writer, revision checks and start exclusion.
Only emit settings supported by the target Minecraft version. Report whether a
Minecraft restart is needed; never restart implicitly. Detect an existing external
pack URL and require an explicit takeover decision in the review. A failed apply
must retain the previous configured release and enough state to reconcile it.

## Phase 2: combining selected packs

Maintain explicit input priority and build one ordinary resource-pack ZIP, not a
ZIP of nested archives. Preview input identities, target compatibility, duplicates,
conflicts and resulting size. Synthesize root metadata deliberately. Initially
combine non-overlapping assets only; report overlapping paths as build blockers.
This restriction must be visible before publication.

Add semantic merging only with a verified, version-specific contract and fixtures.
Language entries and sound-event replacement/append behavior need dedicated rules.
Font providers, atlas sources, filters and overlays require further verification.
A generic last-file-wins merge must not be described as equivalent to Minecraft's
pack stack. Deterministic output ordering, fixed ZIP metadata and unchanged-input
reuse should avoid rebuilding identical releases.

Bound compressed bytes, expanded bytes, entry count, path length, input count,
output size and build time. Reject traversal, linked/special members, duplicate
normalized paths, invalid metadata and unsupported archive features. Choose numeric
limits from actual client-version support and test them explicitly. Stream members
from staged files to the output rather than retaining every pack in RAM. Run at
most one pack build per server with a bounded host-wide budget, outside urgent
control workers. No idle directory scans, periodic rebuilds or bulk rehash polling.

## Phase 3: integration and validation

Expose separate draft, review, job, publication and serving status. Add the hosted
resource-pack destination to Browse only after setup. Resource packs can then be
added to either eligible workspace; only mods offer server-code installation.
Keep publication independent from client modpack revisions and minimum versions.

Use disposable fixtures for unsafe ZIPs, conflicts, format ranges, interrupted
downloads/builds, full disks, concurrent edits, permission revocation and interrupted
configuration publication. Assert old releases stay usable. Test direct downloads
without Vesta authentication, bounded serving under slow clients, correct hashes,
immutable URL caching and management-route isolation. Real Minecraft tests should
cover accepting/declining optional and required packs, reconnects, changed releases,
and older/newer supported versions before claiming compatibility.

Modern clients can stack several server-provided packs through APIs, but ordinary
server.properties describes one pack URL. A Paper/plugin integration for live or
per-player/per-world stacking is a later alternative with explicit ownership rules.

## Existing world-path scope

The current installer reads level-name through the authoritative world observation
and targets that world's datapacks directory. Ordinary Bukkit Nether/End sibling
folders do not receive duplicate datapack installs. Custom universe/world-container
locations outside the adopted game root remain unsupported and must be adopted
explicitly before writes; do not guess their paths or bypass file authorization.
