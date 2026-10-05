package com.vesta.wrapper.identity;

import java.util.List;

/** Schema 4: one server-scoped pack verifier, with a revision-preserving revocation tombstone. */
public final class PackPasswordSchema {
    private PackPasswordSchema() {
    }

    public static List<String> statements() {
        return List.of("""
                CREATE TABLE pack_passwords (
                    server_id TEXT PRIMARY KEY REFERENCES server_identity(server_id),
                    revision INTEGER NOT NULL CHECK (revision >= 1),
                    phc TEXT CHECK (phc IS NULL OR length(phc) BETWEEN 1 AND 256),
                    created_at INTEGER NOT NULL,
                    updated_at INTEGER NOT NULL,
                    revoked_at INTEGER,
                    CHECK ((revoked_at IS NULL AND phc IS NOT NULL)
                        OR (revoked_at IS NOT NULL AND phc IS NULL))
                ) STRICT""");
    }
}
