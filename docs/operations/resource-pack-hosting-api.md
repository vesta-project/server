# Hosted resource-pack API

The backend is separate from the client-modpack workspace. No frontend redesign
or Browse destination is included in this change. Opus can build the interface
against this contract after the practical and regression results are recorded.

## Authority and publication

Management uses the existing canonical device session. Every action currently
requires the Owner role plus `pack.edit` and `config.edit`; the file writer also
checks its existing file permissions. These are server-side policy checks, not
claims accepted from a client. Revocation is checked again by build workers and
the native writer. Requesters never supply a trusted download URL or provider
digest. Provider inputs use server-side Modrinth/CurseForge identity, content-kind,
version compatibility, CDN and hash validation.
CurseForge uses the same bundled application key and trusted runtime overrides
as the content/client-pack adapters (see `pack-workspace.md`); it is never a
management body field.

All management responses use the existing `{data: ...}` / `{error: {code: ...}}`
envelope. Management changes use `POST`, `Content-Type: application/json` and the
existing `Idempotency-Key` header. Keys bind the authenticated principal, device,
route and serialized request. Keep one key for a retry of the same intention.
Do not retry a changed request under that key. Plan tokens are short-lived and
actor-bound; their plaintext is never persisted in receipts or the audit log.
Creating a review plan may issue a fresh ephemeral token on retry; durable replay
applies to mutations such as uploads, draft saves, build admission and apply.

A build produces a private candidate. Knowing its SHA-256 does not grant public
download access. Applying its reviewed plan commits the active release, public
download registration and durable receipt together. A previous applied release
keeps its immutable download URL after subsequent changes. Draft input bytes and
arbitrary files in protected state never have public routes.

## Endpoints

All paths in this table start with `/api/v3/manage/resource-packs`.

| Method and suffix | Body | Result |
| --- | --- | --- |
| `GET` root | none | `Status` |
| `POST /draft` | `Save` | revised `Draft` |
| `POST /uploads/begin` | `{size, sha256}` | `Upload` |
| `POST /uploads/chunk` | `{upload_id, offset, content_base64}` | `Upload` |
| `POST /uploads/finish` | `{upload_id}` | complete `Upload` |
| `POST /uploads/cancel` | `{upload_id}` | boolean |
| `POST /build` | `{expected_revision}` | admitted `Job` |
| `POST /plan` | `{release_id, expected_revision}` | `Plan` |
| `POST /apply` | `{plan_id, confirmation_token, acknowledge_external_pack}` | `Applied` |
| `POST /recovery/abandon` | `{expected_properties_sha256, confirm_abandon: true}` | boolean |

Revisions, upload sizes and offsets are canonical decimal strings. Upload digests
are lowercase SHA-256. Layer IDs are unique lowercase ASCII letters, digits or
hyphens, at most 64 bytes. Order is lowest priority first and highest last.
Unknown request fields are rejected.

```json
{
  "expected_revision": "0",
  "settings": {
    "public_origin": "https://packs.example.com",
    "minecraft_version": "1.21.1",
    "format": {"major": 34, "minor": 0},
    "description": "Our server textures",
    "required": false,
    "prompt": "",
    "layers": [
      {
        "id": "base",
        "source": {
          "type": "provider",
          "provider": "modrinth",
          "project_id": "provider-project-id",
          "version_id": "pinned-provider-version-id"
        }
      },
      {
        "id": "overrides",
        "source": {"type": "upload", "sha256": "<completed-upload-sha256>"}
      }
    ]
  }
}
```

`Draft` has `revision`, stable persisted `pack_id` and nullable `settings`. An
untouched workspace has revision `"0"` and null settings. `Status` contains the
draft, nullable active release, eight newest jobs and `recovery_required`.

`Job` has `id`, `state`, `step`, creation/update timestamps, nullable fixed-code
`error` and nullable `release`. States are `queued`, `running`, `completed`,
`failed` or `interrupted`. Steps include `resolving`, `preparing_inputs`,
`building`, `publishing` and `finished`. Here `publishing` means committing the
private build candidate; public access still requires apply. Query status only
while the workspace is open and a known job is active. There is no background
polling service on the launcher in this change.

`Release` includes `id`, `draft_revision`, `pack_id`, the settings, a compact
artifact descriptor and the future public URL. The artifact has `artifact_name`,
`sha256`, `sha1`, `size` and compact `report`. Full path-resolution provenance is
embedded in `vesta/stack.json` inside the ZIP instead of being duplicated into
historical jobs and every status response.

`Plan` includes `plan_id`, `confirmation_token`, `expires_at_ms`, the release,
`replaces_external_pack` and `restart_needed: true`. It expires after 120 seconds.
If a nonempty current pack URL differs from the last managed active URL, apply
requires `acknowledge_external_pack: true`. The interface must show this takeover
before asking for approval. The plan binds the current full properties hash and
draft revision. Concurrent changes require another review.

`Applied` returns the release and `restart_needed: true`. Nothing starts, stops or
restarts Minecraft implicitly. The existing stopped-game/supervisor hold and
software-maintenance exclusion stay owned through properties writing and durable
publication, so a start or software activation cannot race the two commits.

## Uploads and resource budgets

An upload begins with its exact size and expected SHA-256. Append chunks at the
returned exact byte offset, then finish. Chunks are at most 256 KiB before Base64
encoding. Uploads belong to their creating principal/device until finish; the
finished pinned blob belongs to this hosting workspace. Finishing verifies the
digest and streams chunks into protected disk storage. Archive validation happens
in the build job; finishing an upload alone does not make it a usable release.

There are at most four concurrent uploads, 64 MiB per upload, 1,024 chunks per
upload and 256 MiB combined registered input/upload capacity. Provider downloads
reuse the existing content downloader's 16 MiB per-file bound. Identical provider
selection and target-version pins reuse protected input bytes on subsequent
builds; the builder still verifies their hashes. Downloaded bytes are retained
only after the canonical provider hash matches.

At most one job is queued or running per service, with one demand-driven worker.
No polling, scanning or provider fetching occurs while idle. There are at most
64 job/release records and 4,096 mutation receipts; hitting a cap refuses further
work rather than silently forgetting idempotency or deleting an applied URL.
Automatic expiry/deletion and a UI for explicit storage retirement remain future
work. The process-wide builder permit prevents parallel builds inside an agent;
separate agent processes do not yet share a machine-wide CPU/disk build permit.

The [builder budgets](resource-pack-builder.md) also apply. Job preparation and
metadata/download calls share a 120-second deadline; the remaining time goes to
the builder. Cooperative deadlines cannot interrupt a stuck operating-system
filesystem call. The service owns its worker and database lease through shutdown.
Queued/running jobs found after reopen become `interrupted`; they never retry
downloads or apply settings automatically.

## Public download deployment

The public route is `GET` or `HEAD /resource-packs/<lowercase-sha256>.zip`. It needs
no Vesta session, cookies or custom headers. It serves only explicitly applied
release records. Unknown hashes and private candidates return 404; query strings
are rejected. Responses have `application/zip`, exact length, a hash ETag,
`nosniff` and `Cache-Control: public, max-age=31536000, immutable`.

This route reuses the existing HTTPS listener. Setting `public_origin` does not
bind another listener, provision a certificate, configure DNS, open a firewall or
expose protected state. Configure an ordinary trusted public HTTPS reverse proxy
for **only** `/resource-packs/`, forwarding to the agent's TLS listener with its
certificate explicitly trusted. Do not proxy management routes just to host
packs. The management connection's private certificate pin is not a public Java
client trust chain. An operator must verify the actual public origin before
applying it to a real server.

Downloads share the existing two-artifact global capacity and fixed 64 KiB chunks,
with the existing throughput-floor/connection deadlines. Multiple players behind
the same NAT or reverse proxy can use both slots. The streaming body checks
SHA-256; it does not allocate the entire pack or perform an extra full-file hash
before the first byte. Minecraft receives the SHA-1 in `server.properties`.
Interrupted connections release their capacity. Ranges and conditional 304
responses are not implemented; a reconnect downloads the immutable ZIP again.

## Settings and remaining compatibility work

URL and SHA-1 are emitted for supported release versions. `require-resource-pack`
and the JSON text prompt are emitted from 1.17; stable `resource-pack-id` from
1.20.3. Older versions reject requested prompt/required features. Snapshot strings
are currently rejected instead of guessing their feature cutoffs. The selected
software's authoritative Minecraft version must match the draft at build/apply.

Pack format is currently explicit configuration, checked against every input's
metadata. Automatic target-format lookup from installed software/version metadata
must be added before offering a simple launcher setup flow. A disable/takeover
workspace, explicit storage retirement and native launcher IPC are also not part
of this backend change. The UI must not advertise those as implemented.

A pending properties/publication journal records the before/after full hashes.
Reopen can finish the publication/receipt if the after hash matches, or retire the
attempt if the before hash matches. Any other contents require attention. Existing
active release records remain intact; no unrelated properties edit is rolled back.
The agent remains available for management even when this hosting reconciliation
needs attention, while new Minecraft starts stay blocked. After inspecting the
current properties, the owner can explicitly abandon that uncertain activation.
Use the current full SHA-256 from the existing server-properties read endpoint.
This leaves the file exactly as it is, clears the uncertain managed active pointer
and never deletes earlier public artifacts or starts Minecraft. The owner must
check/fix any currently configured pack URL before starting. A stale hash refuses;
replaying an older abandonment cannot unblock a later pending activation.
This does not certify real Minecraft acceptance/decline behavior,
public certificate deployment, or all client/version combinations.

Primary settings references: [Paper server.properties](https://docs.papermc.io/paper/reference/server-properties/)
and [Minecraft 1.20.3 release notes](https://www.minecraft.net/en-us/article/minecraft-java-edition-1-20-3).
