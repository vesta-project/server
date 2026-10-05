# Vesta server-feature discovery — draft v1

This is a **transport-neutral implementation-discovery contract**, not the complete management API. A non-Java implementation can describe its mechanisms using these schemas and messages. The Java reference codec and the separately implemented Node peer share conformance fixtures, not implementation code. No listener, HTTPS route, action request, authenticated transport, installer, or deployed event stream is introduced.

**Feature support is not authority.** It is not identity proof, permission, certification, process readiness, current runnability, or permission to expose the existing MVP. Authentication and server-local exact authorization remain separate. An advertised UUID does not authenticate the peer; a client must bind a response to server identity obtained from authenticated context. Microsoft/Minecraft proof is still the intended common identity path, not an automatic administrative grant.

## Messages

The fixed identifier is `vesta.server-capabilities`. This is a separate version domain from legacy TCP/API strings, pack versions, and task-event replay versions. Message kind is supplied by the caller/transport, never guessed from fields.

Request (bootstrap envelope does not require a selected version):

```json
{"protocol":"vesta.server-capabilities","versions":[2,1]}
```

`versions` contains 1–8 **unique** integers in 1–65,535. Select the highest intersection with locally implemented versions, regardless of order. Both current peers implement only v1. `[2,1]` selects 1; `[2]` fails, without silently falling back. A client MUST reject a selected version it did not offer or cannot implement. The reference response-consistency helper additionally checks the caller's expected server UUID, but is **not authentication or downgrade protection**.

Manifest:

```json
{"protocol":"vesta.server-capabilities","version":1,"server_id":"12345678-1234-5678-9abc-123456789abc","features":[{"id":"game.stop","support":"unsupported","reason":"unsupported_stop"}]}
```

All fields are required, including explicit `reason:null` for a supported feature. `server_id` is exactly a lowercase canonical UUID string. It must identify the canonical server-local domain, not a legacy configured display/advertised identity. A manifest contains 0–64 unique feature IDs. Duplicate IDs are rejected even if their declarations differ. An absent feature is **not advertised**: clients MUST NOT assume support.

Feature IDs are ASCII, at most 64 characters, and match `[a-z][a-z0-9]*(?:[._-][a-z0-9]+)*` over the **entire** string. No case folding, trimming, prefixes, wildcard, aliases, or namespace inference. Unknown valid IDs are preserved as inert observations and ignored for known-action decisions. They cannot register handlers, create roles/grants, or map themselves into canonical permission IDs. For example, `game.start.extra`, `lifecycle.start`, `security.manage`, and `constructor` do not imply support for `game.start` or permission to start.

`support` is exactly `supported` or `unsupported`. Supported requires explicit null reason. Unsupported requires an ASCII machine code of 1–64 characters matching `[a-z][a-z0-9_]*`. Unknown valid reason codes are tolerated/preserved; they do not alter the support status. No free-form diagnostic messages, paths, commands, scripts, credentials, roles, permissions, or arbitrary extra metadata are accepted.

### Initial feature meanings

`Supported` means the named implementation mechanism exists for the declared configuration. It does **not** mean an action is currently runnable, authorized, remotely accessible, safe to retry, or guaranteed to succeed. No endpoint or transport is advertised by this contract.

| Exact ID | Mechanism described by the current runtime reporter | What is not implied |
|---|---|---|
| `game.start` | Configured literal launch definition and canonical lifecycle start binding | Artifact/runtime/working-directory preflight, readiness, or `lifecycle.start` permission |
| `game.stop` | Configured launch definition with the console-stop mechanism and generation-bound stop binding | Force-kill fallback, completed save, or `lifecycle.stop` permission |
| `game.force_kill` | Configured wrapper-owned generation-bound kill binding | A live process, `lifecycle.force_kill` permission, or satisfied exact confirmation |
| `tasks.replay` | Composed sanitized in-process task replay service | A wire replay endpoint, raw inputs/results/logs, or `tasks.view` permission |
| `game.readiness` | Actual readiness adapter | Process alive/open port is not readiness; currently unsupported |
| `supervisor.continuity` | Game lifetime independently supervised across agent-only failure | Wrapper child-process ownership is not continuity; currently unsupported |

The current reporter uses `not_configured`, `unsupported_stop`, and `not_implemented` reasons. It reports missing-artifact launch configurations as implemented mechanisms, **not ready or executable**: the operation's preflight remains authoritative. Reporting performs no preflight or effects. Peer claims of readiness/continuity are self-reported, not certification by Vesta.

### Failure envelope

Failures are usable before negotiation and never require a selected version or disclose a server ID:

```json
{"protocol":"vesta.server-capabilities","error":"unsupported_protocol"}
```

Fixed codes: `invalid_message` (shape/type/lexical/semantic validation), `message_too_large` (complete byte cap), `unsupported_protocol` (valid version offer has no intersection or selected manifest version is not implemented), `unexpected_response` (otherwise-valid response inconsistent with offered versions/expected server context). No exception text, input, offsets, cause, paths, or free-form message is serialized. A foreign protocol identifier is `invalid_message`. With several defects, the byte cap is checked first; other malformed combinations need not have a universal diagnostic precedence. A receiver must fail closed regardless of which applicable fixed code is used.

## Bounds and canonical encoding

- **Maximum complete message is 16,384 UTF-8 bytes, inclusive**, including whitespace. Check before decoding/parsing; do not truncate. Encoding checks the same bound. The model's ASCII field/count maxima also bound encoder allocation.
- Reject malformed UTF-8 (including encoded surrogates), a leading UTF-8 BOM, invalid JSON, duplicate **decoded** object keys (including escaped aliases), unknown fields at every object level, wrong/null types, and trailing JSON/content. No coercion or recursive skipping of unknown values.
- Integer tokens must use positive decimal digits with no sign, leading zero, fraction, exponent, or quotes. JSON Schema's semantic integer type alone does not enforce this lexical rule.
- Every wire text field is fixed or restricted to its ASCII grammar; escaped lone surrogates, Unicode controls, separators and non-ASCII values therefore fail. Valid whitespace and escaped ASCII within strings are allowed on input.
- Valid member ordering and whitespace are accepted. Canonical output has no whitespace, BOM, or trailing newline. Member order: request `protocol,versions`; manifest `protocol,version,server_id,features`; feature `id,support,reason`; failure `protocol,error`. Versions sort numerically descending; features sort by ascending ASCII ID; identifiers require no escaped characters. All declared unsupported entries remain present, not filtered out.

The schemas use JSON Schema 2020-12. Duplicate decoded keys, numeric spelling, unique feature IDs, total UTF-8 bytes, and response-context binding are **additional normative rules**, not fully representable by these schemas. Conformance tests check the schemas' structure and selected assertions, not a general-purpose JSON Schema engine. No such third-party engine is bundled or certified here.

## Conformance and peer

`v1/fixtures/corpus.json` holds raw JSON or hexadecimal byte recipes and expected canonical output/fixed rejection. Padding recipes test exact complete byte boundaries. Both language implementations independently consume the same physical corpus. Golden files contain canonical bytes with no newline. The Node implementation is not generated from Java and runs without the wrapper:

```sh
node --test scripts/server-feature-protocol.test.mjs
node protocol/peer/server-feature-peer.mjs manifest < protocol/v1/fixtures/manifest.json
```

The CLI accepts `request`, `manifest`, or `failure`, canonicalizes one bounded stdin message, and emits only a fixed failure envelope on refusal (exit 1). It is not a Minecraft server, authentication adapter, HTTP server, or software certification. All new code and tests were authored by the parent session; subagents perform read-only review.

## Evolution and remaining gates

This is an experimental **draft**, not a release promise. Freeze existing feature meanings; adding unknown feature IDs or reason codes fits v1, but new fields/changed semantics need a negotiated version. No released deprecation window exists yet. OpenAPI HTTPS actions, authenticated discovery transport, SSE/WebSocket feeds, complete API idempotency/pagination/resync, OS-local proof, Microsoft UUID proof, device enrollment/mTLS, independent supervisor/IPC, filesystem confinement and signed distribution remain unimplemented or unverified release gates. The initial automatic Vanilla/Paper/Folia/Purpur/Fabric/Quilt/Forge/NeoForge providers and setup wizard remain separate planned work.

Files remain under the repository's existing **GPL-3.0** license. A separately licensed specification/SDK or published repository requires the user's decision; none is created by this checkpoint.
