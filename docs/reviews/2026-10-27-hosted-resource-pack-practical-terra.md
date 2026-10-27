# Hosted resource pack practical HTTPS review, 2026-10-27

Used the opt-in disposable Windows localhost lab and a separate Python HTTPS client validating its fixture CA. Minecraft, EULA, external providers, DNS and a public certificate were not involved. The lab test reported `ok` and exited after its own completion marker. Evidence, with confirmation tokens redacted, is in `V:\tmp\hosted-pack-terra-20261027\evidence.json`.

## Historical finding, resolved in focused recheck

Immediately after the first build completed, and before creating a plan or applying it, unauthenticated GET of its content-addressed artifact returned 200 with the correct ZIP. The same happened for the second build before its apply. Thus a successful build makes its artifact available to someone who knows its hash, even before activation. This conflicts with an expectation that only published packs are public; decide whether build completion intentionally counts as publication or gate downloads on release publication. The raw malformed uploaded input returned 404.

A fresh lab on the updated source verified the fix: unauthenticated GET returned 404 for each of two completed builds before apply and 200 after its respective apply. After the second apply, the first published URL still returned 200. Both downloaded ZIPs matched their declared SHA-256 and SHA-1; HEAD of the first published URL returned 200. The lab test reported `ok` and exited after its completion marker. Redacted evidence is in `V:\tmp\hosted-pack-terra-20261027\recheck-evidence.json`. This practical recheck did not exercise a concurrent server start or software maintenance operation during commit.

## Results

- Owner management GET returned 200; unauthenticated management GET returned 401 `management_session_invalid`.
- Uploaded two ZIPs containing format-34 `pack.mcmeta` and overlapping language JSON. Draft revision 0 saved as revision 1; asynchronous build completed. The merged language file retained both unique keys and the later layer won the shared key. Reversing layer order at revision 2 changed the winning value and the artifact URL.
- Both generated ZIP downloads matched their declared SHA-256 and SHA-1; public GET and HEAD returned 200. After applying the second release, the first URL still downloaded successfully and matched its hashes.
- Plan and apply succeeded for each release. Repeating each apply with its original idempotency key returned the same response. The properties preserved the fixture comment and `motd=Lab`, and contained the expected content-addressed URL, SHA-1, `require-resource-pack=false`, empty prompt, and resource-pack ID. Properties remained unchanged after the malformed build.
- Upload of hash-matching non-ZIP bytes completed, then the build failed with `resource_pack_archive_invalid`, without changing the active properties. A build using stale revision 2 after revision 3 returned 412 `resource_pack_revision_conflict`.

The fixture used `https://packs.example.invalid` as the configured public origin while requests went directly to the local TLS listener. This does not verify public DNS, reverse-proxy routing, real public certificates, Minecraft client behavior, restart behavior, or a UI. The test used small single-chunk uploads and an empty prompt; it does not cover large uploads or nonempty prompt serialization. Replay responses were identical, but no independent side-effect counter was inspected.
