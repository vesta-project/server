-- Service-owned extension tables, installed only during trusted host composition.
CREATE TABLE IF NOT EXISTS software_state (
    singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
    revision INTEGER NOT NULL CHECK(revision >= 0),
    current_launch TEXT NOT NULL,
    previous_launch TEXT,
    recovery_required INTEGER NOT NULL DEFAULT 0 CHECK(recovery_required IN (0,1))
) STRICT;
CREATE TABLE IF NOT EXISTS software_jobs (
    id TEXT PRIMARY KEY,
    kind TEXT NOT NULL CHECK(kind IN ('apply','rollback')),
    state TEXT NOT NULL CHECK(state IN ('queued','staging','activating','succeeded','failed','recovery_required')),
    principal TEXT NOT NULL,
    device TEXT NOT NULL,
    idempotency_key TEXT NOT NULL,
    request_hash TEXT NOT NULL,
    expected_revision INTEGER NOT NULL,
    candidate_launch TEXT,
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    error TEXT,
    UNIQUE(principal,device,idempotency_key)
) STRICT;
