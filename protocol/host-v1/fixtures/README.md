# Native PAKE conformance

`pake-v1.json` is the shared deterministic SPAKE2-0.4.0/Ed25519Group transcript,
including literal canonical transcript bytes and both confirmation MACs. RNG seeds,
password and exporter are public test data; never use these values in deployment.
`negativeCases` lists the executable negative scenarios. Both client and server
tests consume the file. The loopback integration test transports Start/Started and
Finish/Finished JSON through actual TCP/rustls TLS 1.3 streams, with real exporter
material. It covers success, wrong code, exporter mismatch, replay, expired code,
and connection-context substitution; unit tests cover rotation, single-use claim,
capability purpose/device/connection scope, disconnect cleanup, and admission bounds.

Normal targeted commands (set CARGO_TARGET_DIR=V:/server-connections-mvp/target):

```
cargo test -p vesta-host-client --test pake_vectors
cargo test -p vesta-transport host_pake
```

When unrelated workspace resolution is broken, run the standalone harness:

```
cargo test --manifest-path crates/vesta-host-client/tests/conformance/Cargo.toml
```

The harness includes the **actual** `host_pake.rs` and its tests by source path,
and includes the client fixture tests; it does not fork the implementation.
Regenerate the public deterministic fixture, from repository root, with:

```
cargo run --manifest-path crates/vesta-host-client/tests/conformance/Cargo.toml --example generate
```

H04 integration: own `HostPake` behind a mutex; implement `ClaimAuthority` with
atomic durable unclaimed-check/enrollment. `is_unclaimed` is advisory; only
`consume_claim` may commit ownership. Verify the account proof/device signature
before invoking `HostPake::consume_claim`; that call retains the setup password on
provider failure and invalidates its code/exchanges/capabilities only on commit.
Link creation requires owner/local authorization; `consume_link` follows committed
approval of the exact device request. Check `authorize` for every account-proof
and completion call. The returned Capability is internal metadata, never a bearer.
Do not persist it or cache authorization across rotation/disconnect.

H08 integration: obtain `Arc<host_pake::Connection>` from request extensions, use
`Authorization: Pake` only for its documented purpose, map PairingFailed uniformly
to 401 pairing_failed, cap request JSON before typed deserialization, and reject
unknown/duplicate fields using the provided wire structs. Bootstrap on resumed or
TLS 1.2 connections has no Connection extension and must fail. This package does
not register HTTP routes or enable a listener. Setup/link rotation must run through
privileged local/owner control; audit metadata only, never passwords or transcripts.
The native launcher supplies the observed leaf hash and E from the same dedicated
TLS connection to Client::finish; it verifies server confirmation before using the
token, and persists the pin only after final enrollment. Disable TLS resumption
and early data on that bootstrap client. There is no permissive verifier installed
by this crate: the caller must restrict any bootstrap certificate exception to
PAKE traffic only.

The stable SPAKE2 dependency remains unaudited, with upstream timing caveats.
These tests establish interoperability/invariants, not an independent security review.
