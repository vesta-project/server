# Extracted Rust server installer — host-library boundary

Date: **2026-10-08**. Status: **implemented; scoped-reviewed and verified on native Windows/Rust 1.96**.

`vesta-server-install` reuses the companion's reviewed local Rust server installer. It does not
import Piston, Tauri, client profiles, managed Java, authentication or remote action dispatch.
Public plan fields/recipe constructors remain private. Discovery, review consent, a receipt,
progress and process possession do not confer management authority.

## Explicit host configuration

```rust
impl TransportConfig {
    pub fn direct() -> Self;
    pub fn proxy(url: String) -> Result<Self>;
}
impl ServerInstaller {
    pub fn new(roots: InstallRoots, transport: TransportConfig) -> Result<Self>;
    pub fn with_limits(roots: InstallRoots, limits: Limits, transport: TransportConfig)
        -> Result<Self>;
    // Existing versions/plan/install/verify methods and model types retain their behavior.
}
```

`TransportConfig` owns private immutable proxy data and redacts Debug. It has no Default,
serialization, custom-client/origin injection or policy-ID constructor. Direct networking is
an explicit choice and disables ambient proxies. Proxy construction trims and validates the
existing http/https/socks5/socks5h schemes, host and root-only path; query/fragment refuse.
Credentials remain supported but never appear in Debug/errors/plans/receipts. Invalid configuration
returns fixed `TransportFailed`, never a direct fallback. Validation is earlier than the old
request-time parse; that is an intentional fail-before-effects difference, not weaker policy.
The owned transport still generates one private random policy UUID and retains compiled source,
DNS, redirect, budget, checksum, retry and deadline rules. Proxy resolution remains a disclosed
trust boundary, not direct-DNS certification.

## Progress and live control

```rust
pub trait ProgressReporter: Send + Sync {
    fn start_step(&self, name: &str, total_steps: Option<u32>);
    fn update_bytes(&self, transferred: u64, total: Option<u64>);
    fn done(&self, success: bool, message: Option<&str>);
    fn is_cancelled(&self) -> bool;
    fn is_paused(&self) -> bool;
    fn is_dry_run(&self) -> bool { false }
}
```

`SilentProgressReporter` reports nothing and returns false for control checks. Reporters are
trusted host components. Cancellation/pause/dry-run checks stay live throughout an operation;
removing launcher-only notification/action methods does not remove an installer behavior.

The companion facade preserves its existing constructor and method signatures, snapshots
its configured proxy once, and forwards all six methods through a borrowed adapter. There is
no launcher-global configuration lookup inside the extracted crate. `verify` needs no reporter.

## Preserved seams and gaps

The small Java-major parser is private to the runtime module; no Java acquisition machinery is
imported. The installer already has its own non-following ancestor loop that rejects symlinks
and every Windows reparse point. It remains intact; the redundant modpack call is removed.
These checks retain their documented path-race/native-isolation limitations.

Prepared processor/workspace execution remains private staged groundwork; extraction does not
enable Forge/NeoForge or run real provider code. No client-side installer route is introduced.
Trusted authenticated operation/confirmation/recovery/launch-plan composition is still absent.
The original schema hashes, original upstream pins, review consent and receipt-last publication
semantics remain required. No EULA acceptance, automatic stop/restart/world restore, service or
listener is added.

The extracted source keeps edition 2021. Its pinned sysinfo dependency declares Rust 1.88;
the other foundation crates retain their declarations. Neither MSRV is executed here. Design
remains portable; only native Windows tests are authorized in this increment. See
[the checkpoint](RUST_INSTALLER_EXTRACTION_CHECKPOINT.md) for actual evidence and remaining gates.
