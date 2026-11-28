# Host shutdown and restart

A computer or service stop uses an explicit graceful-stop path. An ordinary
separately supervised agent handoff keeps its game running. Host mode always
quiesces and stops its managed games before exiting.

The process registers shutdown notifications before opening listeners or starting
jobs. Windows receives confirmed session-ending notifications through a hidden
native window, and console close/logoff/shutdown notifications through a bounded
handler. A query or canceled session end does not initiate shutdown. Linux handles
SIGTERM and SIGINT on a dedicated signal watcher. Setup wrappers forward shutdown
only to their own still-owned child; they do not restart or roll back into another
host after an OS stop request.

The coordinator closes new request/job admission and scheduling, then asks active
work to reach safe boundaries. Download/copy work becomes interrupted and remains
recoverable. A backup interrupted between chunks never publishes a complete
snapshot. Live restore/activation critical sections retain their journals and
ownership until completion; they are not treated as a successful cancellation.
Idle games may save and stop while endpoints drain; slow metadata requests do not postpone every server. Async HTTP/cache activity drains before per-server databases close.

Every managed server is signaled before the coordinator waits on an individual
server. Games receive the existing fixed graceful stop command through the sole
native owner. OS-driven stop does not record an owner's manual Stop, so existing
start-on-boot/resume policy remains meaningful. A supervised game exit must be
observed or supported by an exact protected final-exit receipt, never inferred
from a missing socket or a reused PID.

Shutdown has one absolute process budget: 60 seconds for normal console/Unix
termination, and 2 seconds for Windows session/console closure. Native Windows
callbacks wait at most 3 seconds, leaving a margin for the watchdog. Repeated
requests cannot extend the budget; a later Windows session-ending request can
shorten an earlier Ctrl+C deadline. Polling multiple workers does not multiply the
budget. The deadline watchdog performs no blocking diagnostic or filesystem IO at
expiry. If cleanup cannot finish, the process exits unsuccessfully without
publishing a clean-stop marker. This is an unclean interruption requiring recovery,
not a guarantee that a stalled Minecraft save completed. Sudden power loss or OS
forced termination cannot be made equivalent to a successful save.

Generated Linux system and user units use SIGTERM, KillMode=mixed,
SendSIGKILL=yes and TimeoutStopSec=90. This gives the host the initial stop signal
and leaves systemd a final process-group fallback after the application's own
budget. Existing installed units are not rewritten by a source change. Explicit
startup configuration may migrate an exact, inactive, Vesta-owned legacy unit;
active or operator-modified units require deliberate handling rather than silent
replacement.

Validation uses disposable files and owned subprocesses. Real Windows restart,
real systemd reboot, and deployment to an existing install require separate owner
approval. Targeted regression checks cover process deadlines (including blocked
stderr), duplicate/canceled notifications, job interruption and recovery,
Minecraft stop/resume intent, supervisor final-exit proof, and unit generation.
