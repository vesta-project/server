CREATE TABLE IF NOT EXISTS hosted_pack_state (
 singleton INTEGER PRIMARY KEY CHECK(singleton=1), revision INTEGER NOT NULL,
 pack_id TEXT NOT NULL, settings TEXT, active TEXT, pending TEXT, recovery INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS hosted_pack_jobs (
 id TEXT PRIMARY KEY, body TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS hosted_pack_releases (
 id TEXT PRIMARY KEY, body TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS hosted_pack_receipts (
 principal TEXT NOT NULL, device TEXT NOT NULL, request_key TEXT NOT NULL,
 digest TEXT NOT NULL, outcome TEXT NOT NULL,
 PRIMARY KEY(principal,device,request_key)
);
CREATE TABLE IF NOT EXISTS hosted_pack_uploads (
 id TEXT PRIMARY KEY, principal TEXT NOT NULL, device TEXT NOT NULL,
 size INTEGER NOT NULL, received INTEGER NOT NULL DEFAULT 0, sha256 TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS hosted_pack_chunks (
 upload_id TEXT NOT NULL REFERENCES hosted_pack_uploads(id) ON DELETE CASCADE,
 offset INTEGER NOT NULL, body BLOB NOT NULL, PRIMARY KEY(upload_id,offset)
);
CREATE TABLE IF NOT EXISTS hosted_pack_blobs (
 sha256 TEXT PRIMARY KEY, size INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS hosted_pack_sources (
 selection_sha256 TEXT PRIMARY KEY, sha256 TEXT NOT NULL REFERENCES hosted_pack_blobs(sha256)
);
CREATE TABLE IF NOT EXISTS hosted_pack_artifacts (
 sha256 TEXT PRIMARY KEY, release_id TEXT NOT NULL REFERENCES hosted_pack_releases(id)
);
