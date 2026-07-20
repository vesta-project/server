package com.vesta.wrapper.auth;

import com.vesta.sync.auth.AuthGate;
import com.vesta.sync.auth.BearerTokenPersistence;
import com.vesta.sync.auth.BearerTokenStore;
import com.vesta.sync.auth.SecurityMode;
import com.vesta.sync.auth.TokenService;
import com.vesta.wrapper.config.WrapperConfig;
import org.slf4j.Logger;
import org.slf4j.LoggerFactory;

import java.nio.file.Path;
import java.util.Optional;
import java.util.UUID;

/**
 * Wrapper-facing auth facade: password login, bearer validation, open-mode warnings.
 * Supports live security-mode/password updates for the local admin panel.
 */
public final class AuthService {

    private static final Logger log = LoggerFactory.getLogger(AuthService.class);

    private volatile AuthGate gate;
    private volatile BearerTokenStore store;
    private volatile SecurityMode mode;
    private volatile String plaintextPassword;
    private volatile long tokenTtlSeconds;
    private final BearerTokenPersistence tokenPersistence;

    public AuthService(WrapperConfig config) {
        this(config, config.scanRoot);
    }

    public AuthService(WrapperConfig config, Path serverRoot) {
        this.tokenTtlSeconds = config.tokenTtlSeconds;
        this.plaintextPassword = config.securityPassword != null ? config.securityPassword : "";
        this.tokenPersistence = serverRoot != null ? new BearerTokenPersistence(serverRoot) : null;
        rebuild(config.securityMode, this.plaintextPassword, this.tokenTtlSeconds, false);
    }

    public SecurityMode mode() {
        return mode;
    }

    public AuthGate gate() {
        return gate;
    }

    public BearerTokenStore store() {
        return store;
    }

    public boolean isOpen() {
        return gate.isOpen();
    }

    public boolean hasPassword() {
        return plaintextPassword != null && !plaintextPassword.isBlank();
    }

    /** Plaintext operator password currently in effect (may be empty). */
    public String currentPassword() {
        return plaintextPassword != null ? plaintextPassword : "";
    }

    public long tokenTtlSeconds() {
        return tokenTtlSeconds;
    }

    public Optional<String> login(String password) {
        Optional<String> token = Optional.ofNullable(gate.issueTokenAfterPassword(password));
        persistTokens();
        return token;
    }

    public Optional<BearerTokenStore.Record> validateBearer(String headerOrToken) {
        if (isOpen()) {
            return Optional.of(new BearerTokenStore.Record("open", "anonymous", null,
                    java.time.Instant.now(), java.time.Instant.now().plusSeconds(3600)));
        }
        String token = AuthGate.extractBearer(headerOrToken);
        return store.validate(token);
    }

    public String issueLinkedToken(UUID uuid) {
        String token = store.issue("linked", uuid);
        persistTokens();
        return token;
    }

    public String issueMinecraftToken(UUID uuid, String name) {
        log.info("Issued Minecraft-session bearer for {} ({})", name, uuid);
        String token = store.issue("linked", uuid);
        persistTokens();
        return token;
    }

    /**
     * Apply a new security mode/password/TTL. Revokes all existing bearers when the
     * mode or password material changes so stale credentials cannot retain access.
     */
    public synchronized void applySecurity(
            SecurityMode nextMode,
            String nextPassword,
            long nextTtlSeconds,
            boolean passwordChanged
    ) {
        SecurityMode mode = nextMode != null ? nextMode : this.mode;
        long ttl = nextTtlSeconds > 0 ? nextTtlSeconds : this.tokenTtlSeconds;
        String password = nextPassword != null ? nextPassword : this.plaintextPassword;
        boolean revoke = passwordChanged
                || mode != this.mode
                || ttl != this.tokenTtlSeconds;
        rebuild(mode, password, ttl, revoke);
    }

    private void rebuild(SecurityMode mode, String password, long ttlSeconds, boolean revokeExisting) {
        this.mode = mode;
        this.plaintextPassword = password != null ? password : "";
        this.tokenTtlSeconds = ttlSeconds;
        TokenService tokens = new TokenService(this.plaintextPassword);
        if (revokeExisting && this.store != null) {
            this.store.revokeAll();
            this.store = new BearerTokenStore(ttlSeconds);
            attachPersistence(this.store, false);
            persistTokens();
        } else if (this.store == null) {
            this.store = new BearerTokenStore(ttlSeconds);
            attachPersistence(this.store, true);
        }
        this.gate = new AuthGate(mode, tokens, this.store);

        if (mode == SecurityMode.OPEN) {
            log.warn("=================================================================");
            log.warn(" SECURITY MODE IS OPEN — anyone who can reach this port can sync");
            log.warn(" (and call management APIs once they exist). Set security.mode =");
            log.warn(" \"password\" (or \"linked\") for production / LAN exposure.");
            log.warn("=================================================================");
        } else if (mode == SecurityMode.PASSWORD && !tokens.hasPassword()) {
            log.error("security.mode=password but security.password is empty — auth will fail");
        } else {
            log.info("Security mode: {}", mode);
        }
    }

    private void attachPersistence(BearerTokenStore nextStore, boolean loadFromDisk) {
        if (tokenPersistence == null) {
            return;
        }
        if (loadFromDisk) {
            tokenPersistence.load(nextStore);
        }
        nextStore.setOnChange(this::persistTokens);
    }

    private void persistTokens() {
        if (tokenPersistence != null && store != null) {
            tokenPersistence.save(store);
        }
    }
}
