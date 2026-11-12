# Optional shared server isolation

The local host document can opt into the same versioned `vesta-sandbox` crate
used by the launcher. No second native sandbox implementation is maintained.
The current development dependency uses the sibling launcher checkout, with an
exact package version. Release distribution still needs an approved immutable
dependency source and inclusion of its matching helper in signed packaging.
Nothing has been published or pushed. Existing installations stay unrestricted
unless their OS administrator explicitly configures this profile.

```toml
[isolation]
profile = "headless"
runtime-directories = ["runtime/jdk"]
```

Runtime directories are explicit, read-only and native-loadable. The game tree
is read/write and native-loadable; only the exact selected executable is in the
process-execution allowlist. Networking remains enabled for a server. Microphone,
USB and desktop integration are withheld. Vesta config, launch documents, TLS
material and canonical state must be outside both trees. Overlaps, including
canonical aliases and protected files that do not exist yet, refuse. All launch
paths (agent, separate supervisor and stopped software activation) compose this
same policy. A software change to a runtime outside the selected directories
refuses rather than broadening the grant. Local isolation policy is part of the
launch identity; a stale supervisor cannot silently retain a different policy.
Remote server Owner permission cannot edit this host setting.

The shared Linux adapter uses bubblewrap and hard-requirement Landlock
Execute/Refer rules. The headless path has a private PID/IPC namespace, minimal
devices, selected JVM libraries and DNS/CA/time files. It omits whole `/etc`,
desktop/audio sockets, inherited desktop environment and parent-death termination.
The trusted supervisor remains outside the sandbox; its existing durable launch
intent/generation records and explicit process-family ownership still apply.
Protected paths beneath implicit library/configuration mounts are also refused.
Windows protected paths beneath ambient Windows/Program Files roots are refused
as well; an administrator's custom broad AppContainer ACL remains outside this
portable policy's guarantee.
No bubblewrap, kernel or helper installation is attempted automatically.

Windows reuses AppContainer, its protected policy, ACL reconciliation, restricted
target and no-child process policy. The sidecar stays outside the restricted
target and inside the server's owned process family. Helper protocol 8 is required
for interactive stdin/Stop forwarding; older helpers refuse admission. Windows'
ambient OS/application ACL grants and ancestor name visibility still apply.
Network-enabled AppContainers have their existing loopback limitation; Vesta does
not add exemptions. This is not VM isolation or a portable resource-quota system.

The sandbox owner/helper PID is retained for generation continuity. It is not
reported as JVM CPU/RAM: native game metrics stay unknown until a trustworthy
restricted-child identity protocol exists. Console/readiness probes are separate.
Temporary policies are retained until actual child exit and all I/O workers join,
including uncertain startup attachment; guard refusal and spawn failure clean
their preparation state. Missing/unsupported enforcement refuses Start with
`game_isolation_unavailable`; it never starts unrestricted while claiming isolation.

Dependency compatibility uses Landlock **0.4.4** with the server's unchanged libc
**0.2.183**. The helper uses Execute/Refer and hard enforcement, not newer ABI 7–9
controls. The launcher still resolves libc 0.2.191 for its existing updater/ZIP/nix
stack; source sharing does not require identical lockfiles. No unrelated package
upgrade or updater downgrade is included.
The existing Rust 1.96 toolchain was used for execution. Rappct declares Rust 1.90;
the new Windows composition is not certified against the server's older declared
toolchain minima. No compiler installation/upgrade was performed.

This document describes implementation, not live Minecraft acceptance. Record
native platform results and unavailable prerequisites separately. Signed helper
packaging, supported-server gameplay/network compatibility, crash continuity with
the native wrapper and quota enforcement require their own evidence.
