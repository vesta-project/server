package com.vesta.sync.auth;

import com.google.gson.Gson;
import com.google.gson.GsonBuilder;
import com.google.gson.reflect.TypeToken;
import org.slf4j.Logger;
import org.slf4j.LoggerFactory;

import java.io.IOException;
import java.io.Reader;
import java.io.Writer;
import java.lang.reflect.Type;
import java.nio.charset.StandardCharsets;
import java.nio.file.AtomicMoveNotSupportedException;
import java.nio.file.Files;
import java.nio.file.Path;
import java.nio.file.StandardCopyOption;
import java.time.Instant;
import java.util.ArrayList;
import java.util.List;
import java.util.UUID;

/**
 * Persists {@link BearerTokenStore} records under {@code .vesta/tokens.json}.
 */
public final class BearerTokenPersistence {

    private static final Logger log = LoggerFactory.getLogger(BearerTokenPersistence.class);
    private static final Gson GSON = new GsonBuilder().setPrettyPrinting().create();
    private static final Type LIST_TYPE = new TypeToken<List<PersistedToken>>() {}.getType();

    private final Path tokensFile;

    public BearerTokenPersistence(Path serverRoot) {
        this.tokensFile = serverRoot.toAbsolutePath().normalize()
                .resolve(".vesta")
                .resolve("tokens.json");
    }

    public Path tokensFile() {
        return tokensFile;
    }

    public void load(BearerTokenStore store) {
        if (!Files.isRegularFile(tokensFile)) {
            return;
        }
        try (Reader reader = Files.newBufferedReader(tokensFile, StandardCharsets.UTF_8)) {
            List<PersistedToken> rows = GSON.fromJson(reader, LIST_TYPE);
            if (rows == null) {
                return;
            }
            int restored = 0;
            Instant now = Instant.now();
            for (PersistedToken row : rows) {
                if (row == null || row.tokenHash == null || row.tokenHash.trim().isEmpty()) {
                    continue;
                }
                Instant expiresAt = parseInstant(row.expiresAt);
                if (expiresAt != null && !now.isBefore(expiresAt)) {
                    continue;
                }
                UUID bound = row.boundUuid != null && !row.boundUuid.trim().isEmpty()
                        ? UUID.fromString(row.boundUuid)
                        : null;
                Instant issuedAt = parseInstant(row.issuedAt);
                if (issuedAt == null) {
                    issuedAt = now;
                }
                if (expiresAt == null) {
                    expiresAt = issuedAt.plusSeconds(86400);
                }
                store.restore(new BearerTokenStore.Record(
                        row.tokenHash,
                        row.role != null ? row.role : "linked",
                        bound,
                        issuedAt,
                        expiresAt
                ));
                restored++;
            }
            store.purgeExpired();
            log.info("Restored {} bearer token(s) from {}", restored, tokensFile);
        } catch (Exception e) {
            log.warn("Could not load bearer tokens from {}: {}", tokensFile, e.getMessage());
        }
    }

    public void save(BearerTokenStore store) {
        try {
            Files.createDirectories(tokensFile.getParent());
            List<PersistedToken> rows = new ArrayList<>();
            for (BearerTokenStore.Record record : store.snapshot()) {
                PersistedToken row = new PersistedToken();
                row.tokenHash = record.tokenHash;
                row.role = record.role;
                row.boundUuid = record.boundUuid != null ? record.boundUuid.toString() : null;
                row.issuedAt = record.issuedAt.toString();
                row.expiresAt = record.expiresAt.toString();
                rows.add(row);
            }
            Path tmp = Files.createTempFile(tokensFile.getParent(), "tokens-", ".json");
            try (Writer writer = Files.newBufferedWriter(tmp, StandardCharsets.UTF_8)) {
                GSON.toJson(rows, writer);
            }
            try {
                Files.move(tmp, tokensFile, StandardCopyOption.REPLACE_EXISTING, StandardCopyOption.ATOMIC_MOVE);
            } catch (AtomicMoveNotSupportedException e) {
                Files.move(tmp, tokensFile, StandardCopyOption.REPLACE_EXISTING);
            }
        } catch (IOException e) {
            log.warn("Could not save bearer tokens to {}: {}", tokensFile, e.getMessage());
        }
    }

    private static Instant parseInstant(String raw) {
        if (raw == null || raw.trim().isEmpty()) {
            return null;
        }
        return Instant.parse(raw);
    }

    static final class PersistedToken {
        String tokenHash;
        String role;
        String boundUuid;
        String issuedAt;
        String expiresAt;
    }
}
