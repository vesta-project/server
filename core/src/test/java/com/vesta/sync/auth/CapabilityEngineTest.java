package com.vesta.sync.auth;

import com.vesta.sync.perm.PermissionChecker;
import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.io.TempDir;

import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.Collections;
import java.util.HashMap;
import java.util.LinkedHashMap;
import java.util.Map;
import java.util.Set;
import java.util.UUID;

import static org.junit.jupiter.api.Assertions.*;

class CapabilityEngineTest {

    private static final UUID PLAYER = UUID.fromString("550e8400-e29b-41d4-a716-446655440000");

    @Test
    void openAllowsPublicDeniesPrivate() {
        AccessControlConfig access = AccessControlConfig.defaults();
        CapabilityEngine engine = new CapabilityEngine(
                access, new PermissionChecker.DenyAll(), u -> false, new OpsLevelProvider.None());
        CapabilityEngine.AuthContext ctx = new CapabilityEngine.AuthContext(
                SecurityMode.OPEN, false, false, null, null);
        assertEquals(CapabilityEngine.Decision.ALLOW, engine.check(AccessCapability.PACK_FETCH, ctx));
        // Password baseline can grant files.read, so unauthenticated callers are asked to auth.
        assertEquals(CapabilityEngine.Decision.DENY_AUTH, engine.check(AccessCapability.FILES_READ, ctx));
        assertEquals(1, engine.effectiveLevel(ctx));
    }

    @Test
    void passwordManageWithoutUuid() {
        AccessControlConfig access = AccessControlConfig.defaults();
        CapabilityEngine engine = new CapabilityEngine(
                access, new PermissionChecker.DenyAll(), u -> false, new OpsLevelProvider.None());
        CapabilityEngine.AuthContext ctx = new CapabilityEngine.AuthContext(
                SecurityMode.PASSWORD, true, true, null, "operator");
        assertEquals(4, engine.effectiveLevel(ctx));
        assertEquals(CapabilityEngine.Decision.ALLOW, engine.check(AccessCapability.CONTENT_MANAGE, ctx));
    }

    @Test
    void whitelistRequiresListedUuidForFiles() {
        AccessControlConfig access = AccessControlConfig.defaults();
        CapabilityEngine engine = new CapabilityEngine(
                access,
                new PermissionChecker.DenyAll(),
                PLAYER::equals,
                new OpsLevelProvider.None());
        CapabilityEngine.AuthContext ok = new CapabilityEngine.AuthContext(
                SecurityMode.WHITELIST, true, false, PLAYER, "linked");
        CapabilityEngine.AuthContext bad = new CapabilityEngine.AuthContext(
                SecurityMode.WHITELIST, true, false, UUID.randomUUID(), "linked");
        assertEquals(CapabilityEngine.Decision.ALLOW, engine.check(AccessCapability.FILES_READ, ok));
        assertEquals(CapabilityEngine.Decision.DENY_WHITELIST, engine.check(AccessCapability.FILES_READ, bad));
        assertEquals(3, engine.effectiveLevel(ok));
        assertEquals(1, engine.effectiveLevel(bad));
    }

    @Test
    void peopleGrantRaisesLevel() {
        Map<UUID, Integer> people = new LinkedHashMap<UUID, Integer>();
        people.put(PLAYER, Integer.valueOf(4));
        AccessControlConfig access = new AccessControlConfig(
                AccessControlConfig.defaults().levels,
                0,
                1,
                4,
                1,
                3,
                people,
                Collections.<String, Integer>emptyMap(),
                Collections.<String>emptyList()
        );
        CapabilityEngine engine = new CapabilityEngine(
                access, new PermissionChecker.DenyAll(), u -> false, new OpsLevelProvider.None());
        CapabilityEngine.AuthContext ctx = new CapabilityEngine.AuthContext(
                SecurityMode.LINKED, true, false, PLAYER, "linked");
        assertEquals(4, engine.effectiveLevel(ctx));
        assertEquals(CapabilityEngine.Decision.ALLOW, engine.check(AccessCapability.CONTENT_MANAGE, ctx));
    }

    @Test
    void permissionNodeGrantRaisesLevel() {
        Map<String, Integer> nodes = new LinkedHashMap<String, Integer>();
        nodes.put("vesta.level.maintainer", Integer.valueOf(3));
        AccessControlConfig access = new AccessControlConfig(
                AccessControlConfig.defaults().levels,
                0,
                1,
                4,
                1,
                3,
                Collections.<UUID, Integer>emptyMap(),
                nodes,
                Collections.<String>emptyList()
        );
        PermissionChecker allowMaintainer = new PermissionChecker() {
            @Override
            public Result check(UUID uuid, String node) {
                return "vesta.level.maintainer".equals(node) ? Result.ALLOW : Result.DENY;
            }
        };
        CapabilityEngine engine = new CapabilityEngine(
                access, allowMaintainer, u -> false, new OpsLevelProvider.None());
        CapabilityEngine.AuthContext ctx = new CapabilityEngine.AuthContext(
                SecurityMode.LINKED, true, false, PLAYER, "linked");
        assertEquals(3, engine.effectiveLevel(ctx));
        assertEquals(CapabilityEngine.Decision.ALLOW, engine.check(AccessCapability.FILES_READ, ctx));
        assertEquals(CapabilityEngine.Decision.DENY, engine.check(AccessCapability.CONTENT_MANAGE, ctx));
    }

    @Test
    void opsJsonLevelMapsDirectly(@TempDir Path root) throws Exception {
        Files.write(
                root.resolve("ops.json"),
                ("[{\"uuid\":\"" + PLAYER + "\",\"name\":\"Player\",\"level\":2,\"bypassesPlayerLimit\":false}]")
                        .getBytes(StandardCharsets.UTF_8)
        );
        AccessControlConfig access = AccessControlConfig.defaults();
        CapabilityEngine engine = new CapabilityEngine(
                access,
                new PermissionChecker.DenyAll(),
                u -> false,
                new OpsJsonReader(root)
        );
        CapabilityEngine.AuthContext ctx = new CapabilityEngine.AuthContext(
                SecurityMode.LINKED, true, false, PLAYER, "linked");
        assertEquals(2, engine.effectiveLevel(ctx));
        assertEquals(CapabilityEngine.Decision.ALLOW, engine.check(AccessCapability.CONSOLE_READ, ctx));
        assertEquals(CapabilityEngine.Decision.DENY, engine.check(AccessCapability.FILES_READ, ctx));
    }

    @Test
    void maxOfBaselinesOpsPeopleAndNodes() {
        Map<UUID, Integer> people = new HashMap<UUID, Integer>();
        people.put(PLAYER, Integer.valueOf(2));
        Map<String, Integer> nodes = new HashMap<String, Integer>();
        nodes.put("vesta.level.admin", Integer.valueOf(4));
        AccessControlConfig access = new AccessControlConfig(
                AccessControlConfig.defaults().levels,
                0,
                1,
                4,
                1,
                3,
                people,
                nodes,
                Collections.<String>emptyList()
        );
        PermissionChecker allowAdmin = new PermissionChecker() {
            @Override
            public Result check(UUID uuid, String node) {
                return "vesta.level.admin".equals(node) ? Result.ALLOW : Result.DENY;
            }
        };
        CapabilityEngine engine = new CapabilityEngine(
                access,
                allowAdmin,
                u -> false,
                new OpsLevelProvider() {
                    @Override
                    public int level(UUID uuid) {
                        return PLAYER.equals(uuid) ? 3 : 0;
                    }
                }
        );
        CapabilityEngine.AuthContext ctx = new CapabilityEngine.AuthContext(
                SecurityMode.LINKED, true, false, PLAYER, "linked");
        assertEquals(4, engine.effectiveLevel(ctx));
        Set<String> caps = engine.capabilities(ctx);
        assertTrue(caps.contains(AccessCapability.CONTENT_MANAGE));
    }

    @Test
    void legacyFacadeStillWorks() {
        SecurityCapabilities caps = SecurityCapabilities.fromLegacy(
                CapabilityLevel.OPEN,
                CapabilityLevel.DENY,
                CapabilityLevel.DENY,
                Collections.<UUID>emptySet(),
                "vesta.mods.install",
                "vesta.files.private",
                false,
                Collections.<String>emptyList());
        CapabilityEngine engine = new CapabilityEngine(
                caps, new PermissionChecker.DenyAll(), u -> false, new OpsLevelProvider.None());
        CapabilityEngine.AuthContext ctx = new CapabilityEngine.AuthContext(
                SecurityMode.OPEN, false, false, null, null);
        assertEquals(CapabilityEngine.Decision.ALLOW, engine.check(CapabilityEngine.Action.PUBLIC_MODPACK, ctx));
        assertEquals(CapabilityEngine.Decision.DENY, engine.check(CapabilityEngine.Action.PRIVATE_RESOURCES, ctx));
    }
}
