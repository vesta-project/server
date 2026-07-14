package com.vesta.wrapper.auth;

import com.vesta.sync.auth.AuthGate;
import com.vesta.sync.auth.BearerTokenStore;
import com.vesta.sync.auth.SecurityMode;
import com.vesta.sync.auth.TokenService;
import com.vesta.wrapper.config.WrapperConfig;
import org.slf4j.Logger;
import org.slf4j.LoggerFactory;

import java.util.Optional;
import java.util.UUID;

/**
 * Wrapper-facing auth facade: password login, bearer validation, open-mode warnings.
 */
public final class AuthService {

    private static final Logger log = LoggerFactory.getLogger(AuthService.class);

    private final AuthGate gate;
    private final BearerTokenStore store;
    private final SecurityMode mode;

    public AuthService(WrapperConfig config) {
        this.mode = config.securityMode;
        TokenService tokens = new TokenService(config.securityPassword);
        this.store = new BearerTokenStore(config.tokenTtlSeconds);
        this.gate = new AuthGate(mode, tokens, store);

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

    public Optional<String> login(String password) {
        String token = gate.issueTokenAfterPassword(password);
        return Optional.ofNullable(token);
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
        return store.issue("linked", uuid);
    }

    public String issueMsaToken(UUID uuid, String name) {
        log.info("Issued MSA-linked bearer for {} ({})", name, uuid);
        return store.issue("linked", uuid);
    }
}
