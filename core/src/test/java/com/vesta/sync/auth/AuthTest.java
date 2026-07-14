package com.vesta.sync.auth;

import org.junit.jupiter.api.Test;

import java.util.UUID;

import static org.junit.jupiter.api.Assertions.*;

class TokenServiceTest {

    @Test
    void verifiesMatchingPassword() {
        TokenService svc = new TokenService("secret");
        assertTrue(svc.hasPassword());
        assertTrue(svc.verifyPassword("secret"));
        assertFalse(svc.verifyPassword("wrong"));
        assertFalse(svc.verifyPassword(null));
    }

    @Test
    void emptyPasswordMeansNoPassword() {
        TokenService svc = new TokenService("");
        assertFalse(svc.hasPassword());
        assertFalse(svc.verifyPassword("anything"));
    }

    @Test
    void opaqueTokensAreUniqueAndHashable() {
        String a = TokenService.issueOpaqueToken();
        String b = TokenService.issueOpaqueToken();
        assertNotEquals(a, b);
        assertEquals(TokenService.hashToken(a), TokenService.hashToken(a));
        assertNotEquals(TokenService.hashToken(a), TokenService.hashToken(b));
    }
}

class BearerTokenStoreTest {

    @Test
    void issueAndValidate() {
        BearerTokenStore store = new BearerTokenStore(3600);
        String raw = store.issue("operator", UUID.randomUUID());
        assertTrue(store.validate(raw).isPresent());
        assertEquals("operator", store.validate(raw).get().role);
        assertFalse(store.validate("bogus").isPresent());
    }

    @Test
    void revokeRemovesToken() {
        BearerTokenStore store = new BearerTokenStore(3600);
        String raw = store.issue("operator", null);
        store.revoke(raw);
        assertFalse(store.validate(raw).isPresent());
    }
}

class AuthGateTest {

    @Test
    void openAllowsWithoutBearer() {
        AuthGate gate = new AuthGate(SecurityMode.OPEN, new TokenService(""), new BearerTokenStore(60));
        assertEquals(AuthGate.Outcome.ALLOW, gate.requireBearer(null));
    }

    @Test
    void passwordModeRequiresBearer() {
        TokenService tokens = new TokenService("hunter2");
        BearerTokenStore store = new BearerTokenStore(3600);
        AuthGate gate = new AuthGate(SecurityMode.PASSWORD, tokens, store);
        assertEquals(AuthGate.Outcome.DENY_MISSING, gate.requireBearer(null));
        String token = gate.issueTokenAfterPassword("hunter2");
        assertNotNull(token);
        assertEquals(AuthGate.Outcome.ALLOW, gate.requireBearer("Bearer " + token));
        assertEquals(AuthGate.Outcome.DENY_WRONG_PASSWORD, gate.loginWithPassword("nope"));
    }

    @Test
    void parseSecurityModes() {
        assertEquals(SecurityMode.OPEN, SecurityMode.parse("open"));
        assertEquals(SecurityMode.PASSWORD, SecurityMode.parse("PASSWORD"));
        assertEquals(SecurityMode.LINKED, SecurityMode.parse("linked"));
        assertThrows(IllegalArgumentException.class, () -> SecurityMode.parse("weird"));
    }
}
