# ReFS installation disk detection

The development server's failed software job recorded
`server_install.storage_unknown`, not `storage_low`. Its installation root is
on V:, a local ReFS volume. Windows disk classification previously allowed only
NTFS. The launcher mapped both errors to the same low-space explanation.

ReFS is now accepted only with a local fixed/removable drive classification and
reported persistent ACL + hard-link support. Windows verification now uses the
full 128-bit FileIdInfo identity and 64-bit volume serial rather than truncating
ReFS identity into the legacy file index. Link-count validation remains enforced.
Older/unsupported volumes still fail closed. The launcher has a separate message
for unverified storage/filesystem support.

Capacity policy remains unchanged: reserve 512 MiB beyond installation needs and
other active in-process reservations. JAR installs reserve their published size
(or the 1 GiB artifact ceiling when unknown). Loader bundles sum their inputs;
processed Forge installs reserve twice the bounded work-tree budget (at most
8 GiB plus the 512 MiB floor). Thus the observed 13.3 GiB free was not the cause
of this failure. No world/server-folder backup is created by a software switch.
The previous immutable installation remains in place for rollback; cross-version
library deduplication is not currently implemented.

Validation: 35 installation tests pass. The opt-in selected-volume fixture was
also explicitly run on V:/tmp and passed space detection, full file identity,
hard-link creation, duplicate-link refusal and identity stability after unlink.
The fixture owns and removes only its temporary directory. Strict installer
Clippy passes. No live agent, Minecraft, installation or recovery record changed.

Primary platform references:
- https://learn.microsoft.com/en-us/windows-server/storage/refs/refs-overview
- https://learn.microsoft.com/en-us/windows/win32/api/fileapi/ns-fileapi-file_id_info
