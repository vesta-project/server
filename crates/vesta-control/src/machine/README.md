# Host state integration

`HostState::open` uses a dedicated host-private directory and the existing
`vesta_state::StateDatabase` for file protection, WAL, full synchronous writes,
immediate transactions, snapshots and rollback. Do not point it at a server's
state directory. The storage engine's legacy `server_identity` record is unused
by host APIs; `machine_host.host_id` is the host identity. The host schema has its
own version, independent of the storage engine's migration version.

H02 should retain account/device ids in sessions and call `active_device` (or
`owners::active_device_in` on an existing transaction) and resolve current roles
for every operation. Stored generation values are counters, not authority.

H04 must verify PAKE/account proof/signatures before enrollment. Use
`HostState::write` with `HostState::claim_in` or `HostState::enroll_device_in` so
code/proof consumption and enrollment share one transaction. Returning an error
rolls everything back. These trusted repository APIs do not authenticate a UUID,
an `Actor`, a device key, a local peer, or a signature. `LocalRecovery` may only
come from the privileged local channel. Transfer requires the caller's fresh
signed primary-device confirmation; the target is already an owner and the old
primary remains an owner. All authority mutations write a durable audit record.

H08 should use `visible_servers` and `effective_role`, with server ids selected
before dispatch. `server` is a trusted lookup and does not authorize access.
Anonymous public lookup is a separate later package; public visibility never
grants management authority. Role and registry changes require the current
revision. Device revocation is repeatable and protects all primary-account keys.
Registry capacity is one active entry; deletion leaves an id tombstone. Owners
resolve access dynamically, including future entries, rather than copying grants.

Counters are positive signed SQLite integers exposed as `u64`; exhaustion fails
and rolls back instead of wrapping. Transport should encode them as decimal
strings. Every authority change advances the host-wide generation. Worker
synchronization/barriers, wire idempotency, PAKE, online account proof and sessions
belong to their respective packages and are not implemented here. Reserved module
files are intentionally empty extension points.
