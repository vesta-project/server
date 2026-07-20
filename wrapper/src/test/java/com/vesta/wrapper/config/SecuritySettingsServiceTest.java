package com.vesta.wrapper.config;

import com.vesta.sync.auth.AccessCapability;
import com.vesta.sync.auth.AccessControlConfig;
import com.vesta.sync.auth.CapabilityEngine;
import com.vesta.sync.auth.SecurityMode;
import com.vesta.sync.perm.PermissionChecker;
import com.vesta.wrapper.auth.AuthService;
import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.io.TempDir;

import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import java.util.Set;
import java.util.UUID;

import static org.junit.jupiter.api.Assertions.*;

class SecuritySettingsServiceTest {

    @TempDir
    Path root;

    @Test
    void rewritePreservesNonSecuritySectionsAndAppliesLive() throws Exception {
        Path config = root.resolve("wrapper-config.toml");
        Files.writeString(config, """
                [server]
                http-port = 9090
                scan-root = "."

                [security]
                mode = "open"
                password = ""
                token-ttl-seconds = 86400
                control-port = 9091
                anonymous-level = 1
                authenticated-level = 1
                password-level = 4
                linked-level = 1
                whitelist-level = 3
                """, StandardCharsets.UTF_8);

        WrapperConfig wrapperConfig = new WrapperConfig.Builder()
                .scanRoot(root)
                .securityMode(SecurityMode.OPEN)
                .controlPort(9091)
                .build();
        AuthService auth = new AuthService(wrapperConfig);
        CapabilityEngine engine = new CapabilityEngine(
                wrapperConfig.capabilities.access,
                new PermissionChecker.AllowAll(),
                uuid -> false,
                new com.vesta.sync.auth.OpsLevelProvider.None()
        );
        SecuritySettingsService service = new SecuritySettingsService(config, auth, engine, 9091);

        UUID player = UUID.randomUUID();
        Map<String, Object> body = new LinkedHashMap<>();
        body.put("mode", "password");
        body.put("password", "secret");
        body.put("anonymous_level", 0);
        body.put("people", List.of(Map.of("uuid", player.toString(), "level", 4)));

        Map<String, Object> snapshot = service.update(body);
        assertEquals("password", snapshot.get("mode"));
        assertEquals(true, snapshot.get("password_configured"));
        assertEquals(true, snapshot.get("access_requires_auth"));
        assertEquals(SecurityMode.PASSWORD, auth.mode());
        assertEquals(0, engine.config().anonymousBaseline);
        assertEquals(4, engine.config().people.get(player).intValue());

        String text = Files.readString(config, StandardCharsets.UTF_8);
        assertTrue(text.contains("[server]"));
        assertTrue(text.contains("http-port = 9090"));
        assertTrue(text.contains("mode = \"password\""));
        assertTrue(text.contains("password = \"secret\""));
        assertTrue(text.contains('"' + player.toString() + "\" = 4"));

        // Token issued before password change should be revoked after a mode/password update.
        String token = auth.login("secret").orElseThrow();
        assertTrue(auth.validateBearer(token).isPresent());
        service.update(Map.of("password", "changed"));
        assertTrue(auth.validateBearer(token).isEmpty());
        assertTrue(auth.login("changed").isPresent());
    }

    @Test
    void passwordModeRequiresPassword() throws Exception {
        Path config = root.resolve("wrapper-config.toml");
        Files.writeString(config, "[security]\nmode = \"open\"\npassword = \"\"\n", StandardCharsets.UTF_8);
        WrapperConfig wrapperConfig = new WrapperConfig.Builder().scanRoot(root).build();
        AuthService auth = new AuthService(wrapperConfig);
        CapabilityEngine engine = new CapabilityEngine(
                AccessControlConfig.defaults(),
                new PermissionChecker.AllowAll(),
                uuid -> false,
                new com.vesta.sync.auth.OpsLevelProvider.None()
        );
        SecuritySettingsService service = new SecuritySettingsService(config, auth, engine, 9091);
        IllegalArgumentException error = assertThrows(
                IllegalArgumentException.class,
                () -> service.update(Map.of("mode", "password", "clear_password", true))
        );
        assertTrue(error.getMessage().contains("password"));
    }

    @Test
    void rewriteSecuritySectionsBuildsValidTables() {
        AccessControlConfig access = new AccessControlConfig(
                AccessControlConfig.defaults().levels,
                0, 1, 4, 1, 3,
                Map.of(UUID.fromString("00000000-0000-0000-0000-000000000001"), 4),
                Map.of("vesta.level.admin", 4),
                List.of("config/private")
        );
        String out = SecuritySettingsService.rewriteSecuritySections(
                "[modpack]\nname = \"Demo\"\n",
                SecurityMode.LINKED,
                "",
                3600,
                9091,
                access
        );
        assertTrue(out.contains("[security]"));
        assertTrue(out.contains("mode = \"linked\""));
        assertTrue(out.contains("[security.people]"));
        assertTrue(out.contains("[security.perm-nodes]"));
        assertTrue(out.contains("[security.levels]"));
        assertTrue(out.contains('"' + AccessCapability.PACK_FETCH + '"'));
        assertTrue(out.contains("[modpack]"));
    }
}
