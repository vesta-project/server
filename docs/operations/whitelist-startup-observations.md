# Whitelist startup observations — vanilla 26.3

Implementation follow-up: [queued desired membership](queued-whitelist.md) now handles startup edits without writing Minecraft's file. The observations below remain the evidence from the earlier isolated testing pass.

Tested on 2026-10-23 with the locally installed vanilla 26.3 server (protocol 777), Java 25, a fresh temporary world and only a loopback listener. Existing explicitly accepted EULA text was copied into the fixture. No development instance files, world, agent or running game were changed. Heap was capped at 512 MiB and the JVM at two active processors.

Reproducer: `scripts/test-whitelist-startup.ps1`, supplying the unpacked vanilla server JAR, its existing libraries directory and an already accepted EULA file. It creates a new temporary directory, records console/error logs and writes `result.json`. It rejects an unobserved startup injection and always stops its own child. No external player-profile lookup or game client is required.

## Observed behavior

1. Before process launch, seed `vestabefore` and `vestaremove` in `whitelist.json`.
2. On the `Preparing level` log line, before `Done`, rewrite the file to contain `vestabefore` and `vestaduring`: one addition and one removal.
3. After readiness, `whitelist list` still returns `vestabefore, vestaremove`. Minecraft had already loaded the original file; the startup file edit did not update memory.
4. Run `whitelist add vestaafter`. The saved file contains `vestabefore, vestaremove, vestaafter`. The startup file addition disappears and its removal is undone. A command saves the in-memory whitelist over the external edit.
5. Run `whitelist add vestaduring` and `whitelist remove vestaremove` after readiness. The list correctly becomes `vestabefore, vestaafter, vestaduring`.
6. Replace the file with only `vestabefore`. The live list remains unchanged until `whitelist reload`, after which it contains only `vestabefore`.

Two clean isolated runs reproduced these results and exited normally. The checked-in `whitelist-startup-26.3-result.json` preserves the latest successful command/file observations. Local bytecode inspection agrees: `DedicatedPlayerList` loads the whitelist in construction and calls the same loading routine for reload; the actual observed timing is before the world-preparation marker in this version.

## Identity caveat discovered by testing

An initial mixed-case fixture exposed another issue: the offline-mode command profile resolver in this clean server stored `VestaCaseProbe` as `vestacaseprobe`, UUID `da733afd-1532-348f-adca-1508a4e30052`. Removing a directly seeded mixed-case offline UUID using the name-based command did not remove that entry. Bytecode inspection of `CachedUserNameToIdResolver` shows lowercased lookup and offline profile construction.

The timing tests use lowercase names to isolate the startup race from this identity difference. This does not prove actual mixed-case client admission behavior: no client login was performed. Do not change the direct-file UUID algorithm to lowercase based only on command output, and do not assume running command resolution and a direct offline UUID are interchangeable. Mixed-case login/UUID behavior and removal by an already stored UUID need a separate compatibility test before startup reconciliation is implemented for vanilla offline-mode servers. The existing stopped editor remains stopped-only.

## Recommended startup contract

- Stopped with proven ownership: keep the implemented guarded, recoverable file edit.
- Starting: durably record a desired whitelist change in Vesta and display it as pending. Do not report confirmed membership and do not concurrently rewrite Minecraft's whitelist file.
- Ready: apply only those pending structured actions under current authority, with idempotency and process-generation/identity checks; observe the result rather than interpreting stdin delivery as proof. Confirmed application, unknown outcome and cancelled/blocked startup must remain distinct.
- Preserve ordering for add/remove of the same player; retain or explicitly cancel unresolved intents after failed startup. Do not silently replay an old generation's command into an unrelated replacement process.

Writing the file and later issuing a command can eventually converge in a simple case, but it introduces an observable, overwrite-prone intermediate state. The test demonstrates why a Vesta-owned pending intent is a better source of desired state. `whitelist reload` works here, but is not an automatic replacement for scoped changes: it reloads the whole file and may interact with external edits/admission policy.

This is evidence for vanilla 26.3 only. Paper, older vanilla versions, mod loaders, profile caches and forwarding/custom-auth configurations are not certified. No startup queue or production behavior was changed in this testing pass.
