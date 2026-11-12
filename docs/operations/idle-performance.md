# Idle automation measurements, 24 October 2026

An empty automation pulse now checks a fresh query-only snapshot before requesting
writer admission. It still uses the normal durable write path whenever an enabled
workflow, queued/running run, expired history or retention overflow exists. This
does not cache policy or authority and does not change retention cutoffs. Work
admitted concurrently with an empty snapshot is observed on the next pulse.

The gate is deliberately conservative: even enabled manual/event workflows still
take the write path. Avoiding that work or calculating schedule deadlines would
need separate measurements and race coverage.

## Reproduction and observations

```powershell
cargo build --locked --release -p vesta-agent --bin vesta-agent
./scripts/measure-idle-agent.ps1 -Seconds 20
cargo test --locked -p vesta-control --lib automation::tests -- --test-threads=2
```

The existing harness owns a fresh temporary fixture and matching child PID. It
contains no runnable game, has no clients, uses loopback TLS and pins the agent to
one logical CPU. It preserves logs and stops only its own matching child. It does
not touch the development agent or server data.

Windows 11 build 26100, Ryzen 7 6800H, release build, two-second warmup, 20-second
samples. These are desktop observations, not acceptance on a 1-core/2-GB host.

| Build/sample | CPU seconds | Percent of one core | Private MiB |
| --- | ---: | ---: | ---: |
| Before, initial | 0.484375 | 2.421 | 3.910 |
| Before, paired | 0.421875 | 2.108 | 3.660 |
| After, paired | 0.343750 | 1.718 | 3.875 |
| After, repeat | 0.375000 | 1.874 | 3.918 |
| Before, repeat | 0.437500 | 2.187 | 3.895 |

The before binary was built from `5073b1b`; the after binary includes the gate in
this change. SHA-256 fingerprints:

- Before: `2900BEA9FF9AA810AEE97923B39669E16CCA28096C6C841A38AD201E2DC2BDEC`.
- After: `4858103B602AF2A6CDC8EB229E00D4F4972AD5537BD824BFC1CC549FCE6AB3F7`.

The paired observations suggest lower idle CPU. Host load, CPU frequency and timer
resolution were not controlled; no universal percentage gain is claimed. The
earlier sub-1%-of-one-core target remains unmet. Twelve threads remained present;
there is no new thread, connection pool, database schema or durability setting.

## Validation and boundaries

All 35 automation tests pass, including a held-writer regression proving an empty
pulse finishes without joining the writer queue, newly admitted work, closed-state
refusal, and history/notification expiry without enabled workflows. Existing
authority, scheduling, parallel-driver and crash-reconciliation tests still pass.
Strict all-target control Clippy passes. The separate console capture/adversarial/
concurrency suites pass 13 tests.

There is no Minecraft load, real low-end storage, Linux/ARM, memory cap or network
impairment result here. Reserved control admission still needs current authority,
tokens and a responsive agent/game; this gate does not bypass those checks.
