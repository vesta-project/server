# Hosting management implementation and verification

World, software and SFTP management now have server routes, native launcher
commands and launcher UI. The launcher checkpoint is `23bb0bf1`; server integration
is checkpointed at `57a57cc` with line endings normalized in `cc84d52`.

## Behavior

- World settings distinguish the saved 64-bit seed from the seed for new worlds.
  Exact-name confirmation and a stopped-state hold select a fresh world through
  one recoverable properties replacement. Existing worlds remain in place.
- Software plans stage verified installations side by side. Apply and explicit
  rollback replace persisted and actual runtime/supervisor launch definitions
  while stopped. Interrupted activation blocks starts until deliberate recovery.
  Neither action stops or starts Minecraft. Vanilla, Paper, Folia, Purpur, Fabric
  and Quilt have managed recipes; Forge/NeoForge are visibly unavailable.
- Files has an SFTP button that uses a native-built URL for the trusted endpoint.
  Setup/details/key management are on demand. SSH is off by default, Ed25519
  public-key only, with current canonical permissions checked on each request.
  There is no shell or port forwarding. Uploads retain one recovery original,
  use bounded direct chunks, and reject stale writers. Management receipts prevent
  duplicate effects after lost responses; interrupted pending receipts fail closed.

## Verification on Windows

- SFTP service: 8 focused tests pass, including the installed OpenSSH client with
  host-key pinning, 2 MiB uploads, one recovery copy, concurrent-writer refusal,
  create-only authority, revocation, expiry, malformed packets and owned shutdown.
- Software service: 8 tests pass, including interrupted activation, rollback,
  durable idempotency, revoked owner and live-game refusal.
- World/parser tests pass, including first start without server.properties,
  signed 64-bit precision, preserved data, exact replay, conflicts and live refusal.
- Installer managed fixtures: 4 filtered tests pass, covering all six providers,
  absolute loader paths and rollback artifact tamper refusal.
- Real supervisor stopped launch replacement passes its protected-endpoint test.
- HTTPS hosting routes pass owner/authentication, new-world and SFTP key replay
  integration tests. Launcher client management: 18/18 pass, including native-only
  tokens, exact retry bodies, 64-bit revisions and authority rechecks on replay.
- Native launcher library check and agent all-target check pass. Client strict
  Clippy passes. Server scoped Clippy passes with collapsible-if exempted because
  raising the declared MSRV to 1.89 exposes that style lint in existing modules.
- UI typecheck passes; hosting/shell tests pass (36), and final hosting/localization
  tests pass (16). Lint completes with warnings, including reactive-scope warnings.
  Fixture browser interactions were checked by the UI agent, but no screenshots
  were captured. No packaged/native visual sign-off is claimed.
- Broad control library run: 974 passed, 3 failed. Decompression TTL, player-query
  fixture readiness and sleep login burst each pass individually with one test
  thread. The parallel run remains recorded as failed; do not count it as clean.

## Remaining platform and feature limits

SFTP files are capped at 16 MiB, matching the existing recoverable browser writer.
Large world archives need a streaming recovery design before raising this ceiling.
Same-account direct filesystem races are outside the confinement guarantee.
Ed25519 is the only accepted SSH key algorithm. Remote Linux SSH, packaged
launcher/default-app behavior, and real Minecraft version switching still need
live host validation. No running development agent, Minecraft process, real world,
external release or production installation was changed by these tests.
