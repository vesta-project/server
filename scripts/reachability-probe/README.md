# External reachability probe

Run this stateless Node.js 22 HTTPS service on a public host outside the Vesta server's network. It accepts `POST /v1/check` with exactly `{ "schema": 1, "port": 443, "installation_id": "<32 lowercase hex>", "tls_sha256": "<64 lowercase hex>", "nonce": "<32 lowercase hex>" }`. The target IP is always the request's TCP peer address. Private, loopback, reserved, and non-global peer addresses are rejected; forwarded-IP headers and caller-supplied addresses are ignored.

Configure `TLS_CERT_FILE` and `TLS_KEY_FILE` with the probe endpoint's HTTPS certificate and private key. Optionally set `BIND` (default `0.0.0.0`) and `PORT` (default `9443`), then run:

```sh
TLS_CERT_FILE=/etc/vesta-probe/tls.crt TLS_KEY_FILE=/etc/vesta-probe/tls.key node scripts/reachability-probe/server.mjs
```

The service caps request bodies at 2 KiB, runs at most four probes at once, and applies a 30-second per-peer cooldown with a bounded cache. Each outbound check connects directly to the peer IP and supplied port, verifies the leaf certificate SHA-256 pin, then requests `/api/v3/setup`. It reports reachable only for HTTP 200 with the matching `installation_id` and `status: "awaiting_app_onboarding"`; the check has a four-second deadline. It issues no credentials and stores no installation data between requests.

The exported `createProbeServer({ key, cert, check })` creates an HTTPS server without listening. `productionCheck` is exported for focused tests. Importing the module has no network or filesystem side effects; only direct execution reads the TLS files and listens.
