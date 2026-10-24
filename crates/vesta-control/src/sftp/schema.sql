CREATE TABLE IF NOT EXISTS sftp_configuration (
    server_id TEXT PRIMARY KEY REFERENCES server_identity(server_id),
    config_json TEXT NOT NULL CHECK(length(config_json)<=8192)
) STRICT;
CREATE TABLE IF NOT EXISTS sftp_management_outcomes (
    server_id TEXT NOT NULL REFERENCES server_identity(server_id),
    request_key TEXT NOT NULL,
    principal_id TEXT NOT NULL,
    device_id TEXT NOT NULL,
    input_hash TEXT NOT NULL,
    result_json TEXT CHECK(result_json IS NULL OR length(result_json)<=65536),
    PRIMARY KEY(server_id,request_key)
) STRICT;
CREATE TABLE IF NOT EXISTS sftp_keys (
    id TEXT PRIMARY KEY REFERENCES devices(id),
    server_id TEXT NOT NULL REFERENCES server_identity(server_id),
    principal_id TEXT NOT NULL REFERENCES principals(id),
    label TEXT NOT NULL CHECK(length(label)<=96),
    public_key TEXT NOT NULL CHECK(length(public_key)<=4096),
    fingerprint TEXT NOT NULL UNIQUE,
    created_at INTEGER NOT NULL,
    expires_at INTEGER,
    revoked_at INTEGER
) STRICT;
CREATE TABLE IF NOT EXISTS sftp_host_key (
    server_id TEXT PRIMARY KEY REFERENCES server_identity(server_id),
    private_key TEXT NOT NULL CHECK(length(private_key)<=8192),
    fingerprint TEXT NOT NULL
) STRICT;
CREATE TABLE IF NOT EXISTS sftp_transfer_usage (
    server_id TEXT NOT NULL REFERENCES server_identity(server_id),
    principal_id TEXT NOT NULL REFERENCES principals(id),
    day INTEGER NOT NULL,
    bytes INTEGER NOT NULL CHECK(bytes>=0),
    PRIMARY KEY(server_id,principal_id)
) STRICT;
