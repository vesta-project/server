# Pack downloads

Server modpack installs use a bounded parallel downloader for provider files.
The same path handles supported providers, verifies each provider's pinned hash,
and stages payloads before changing the game directory. Client-only resources,
resource packs and shaders continue to be excluded from server installations.

Host TOML configuration can override concurrency:

```toml
[downloads]
max-concurrent = 4
```

Valid explicit limits are 1 through 16. Restart the agent after editing its host
configuration. Omit the field (or the entire table) for automatic mode.

Automatic mode starts with two transfers. Its ceiling is based on available CPU
parallelism, capped at eight. Between batches it adjusts concurrency using actual
verified transfer throughput. Recent host metrics lower the limit under high CPU
usage or low available memory. If metrics are disabled or unavailable, it uses
CPU capacity and observed transfer throughput. It does not run speed tests or
infer internet bandwidth from a network adapter's advertised link speed.

Progress combines bytes and completed files across workers. Workers are scoped
to the install; all active workers finish before a failure escapes. A failed
batch cannot be committed as a successful partial install. Download responses
still obey the existing source, size, hash and deadline checks.
