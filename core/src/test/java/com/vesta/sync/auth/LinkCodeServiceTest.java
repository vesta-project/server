package com.vesta.sync.auth;

import org.junit.jupiter.api.Test;

import java.util.UUID;

import static org.junit.jupiter.api.Assertions.*;

class LinkCodeServiceTest {

    @Test
    void redeemOnce() {
        LinkCodeService svc = new LinkCodeService(60);
        UUID id = UUID.randomUUID();
        String code = svc.createCode(id);
        assertEquals(id, svc.redeem(code).get());
        assertFalse(svc.redeem(code).isPresent());
    }

    @Test
    void noteExternalWorks() {
        LinkCodeService svc = new LinkCodeService(60);
        UUID id = UUID.randomUUID();
        svc.noteExternal("AB12CD", id);
        assertEquals(id, svc.redeem("ab12cd").get());
    }
}
