package com.vesta.sync.auth;

import com.vesta.sync.perm.PermissionChecker;
import org.junit.jupiter.api.Test;

import java.util.Collections;
import java.util.UUID;

import static org.junit.jupiter.api.Assertions.*;

class CapabilityEngineTest {

    private static final UUID PLAYER = UUID.fromString("550e8400-e29b-41d4-a716-446655440000");

    @Test
    void openAllowsPublicDeniesPrivate() {
        SecurityCapabilities caps = new SecurityCapabilities(
                CapabilityLevel.OPEN,
                CapabilityLevel.DENY,
                CapabilityLevel.DENY,
                Collections.emptySet(),
                "vesta.mods.install",
                "vesta.files.private",
                false,
                Collections.emptyList());
        CapabilityEngine engine = new CapabilityEngine(caps, new PermissionChecker.DenyAll(), u -> false, u -> false);
        CapabilityEngine.AuthContext ctx = new CapabilityEngine.AuthContext(
                SecurityMode.OPEN, false, false, null, null);
        assertEquals(CapabilityEngine.Decision.ALLOW, engine.check(CapabilityEngine.Action.PUBLIC_MODPACK, ctx));
        assertEquals(CapabilityEngine.Decision.DENY, engine.check(CapabilityEngine.Action.PRIVATE_RESOURCES, ctx));
    }

    @Test
    void passwordManageWithoutUuid() {
        SecurityCapabilities caps = SecurityCapabilities.defaults();
        CapabilityEngine engine = new CapabilityEngine(caps, new PermissionChecker.DenyAll(), u -> false, u -> false);
        CapabilityEngine.AuthContext ctx = new CapabilityEngine.AuthContext(
                SecurityMode.PASSWORD, true, true, null, "operator");
        assertEquals(CapabilityEngine.Decision.ALLOW, engine.check(CapabilityEngine.Action.MANAGE_CONTENT, ctx));
    }

    @Test
    void whitelistRequiresListedUuid() {
        SecurityCapabilities caps = new SecurityCapabilities(
                CapabilityLevel.WHITELIST,
                CapabilityLevel.WHITELIST,
                CapabilityLevel.DENY,
                Collections.emptySet(),
                "vesta.mods.install",
                "vesta.files.private",
                false,
                Collections.emptyList());
        CapabilityEngine engine = new CapabilityEngine(
                caps,
                new PermissionChecker.DenyAll(),
                PLAYER::equals,
                u -> false);
        CapabilityEngine.AuthContext ok = new CapabilityEngine.AuthContext(
                SecurityMode.WHITELIST, true, false, PLAYER, "linked");
        CapabilityEngine.AuthContext bad = new CapabilityEngine.AuthContext(
                SecurityMode.WHITELIST, true, false, UUID.randomUUID(), "linked");
        assertEquals(CapabilityEngine.Decision.ALLOW, engine.check(CapabilityEngine.Action.PRIVATE_RESOURCES, ok));
        assertEquals(CapabilityEngine.Decision.DENY_WHITELIST, engine.check(CapabilityEngine.Action.PRIVATE_RESOURCES, bad));
    }
}
