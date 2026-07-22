package com.vesta.wrapper.server;

import com.google.gson.JsonObject;
import com.google.gson.JsonParser;
import com.vesta.sync.VestaSync;
import com.vesta.sync.auth.CapabilityEngine;
import com.vesta.sync.auth.MinecraftSessionChallengeService;
import com.vesta.sync.auth.OpsLevelProvider;
import com.vesta.sync.auth.RealtimeTicketService;
import com.vesta.sync.auth.SecurityMode;
import com.vesta.sync.perm.PermissionChecker;
import com.vesta.wrapper.auth.AuthService;
import com.vesta.wrapper.config.SecuritySettingsService;
import com.vesta.wrapper.config.WrapperConfig;
import com.vesta.wrapper.security.TlsMaterial;
import com.vesta.wrapper.security.WhitelistService;
import com.vesta.wrapper.runtime.MinecraftServerProcess;
import com.vesta.sync.auth.LinkCodeService;
import org.junit.jupiter.api.AfterEach;
import org.junit.jupiter.api.BeforeEach;
import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.io.TempDir;

import javax.net.ssl.SSLContext;
import javax.net.ssl.TrustManager;
import javax.net.ssl.X509TrustManager;
import java.net.URI;
import java.net.http.HttpClient;
import java.net.http.HttpRequest;
import java.net.http.HttpResponse;
import java.security.SecureRandom;
import java.security.cert.X509Certificate;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.time.Duration;
import java.util.Optional;
import java.util.UUID;

import static org.junit.jupiter.api.Assertions.*;

class VestaHttpServerTlsFingerprintTest {

    @TempDir
    Path root;

    private VestaHttpServer server;
    private HttpClient client;
    private URI base;

    @BeforeEach
    void startServer() throws Exception {
        Path configDirectory = Files.createDirectories(root.resolve("config"));
        Files.writeString(configDirectory.resolve("vesta-test.txt"), "tls fixture", StandardCharsets.UTF_8);

        Path configFile = root.resolve("wrapper-config.toml");
        Files.writeString(configFile, """
                [security]
                mode = "linked"
                password = ""
                token-ttl-seconds = 86400
                control-port = 9091
                anonymous-level = 0
                authenticated-level = 1
                password-level = 4
                linked-level = 1
                whitelist-level = 3
                """, StandardCharsets.UTF_8);

        UUID serverUuid = UUID.randomUUID();

        WrapperConfig config = new WrapperConfig.Builder()
                .scanRoot(root)
                .scanPaths(java.util.List.of("config"))
                .httpPort(0)
                .tcpPort(9092)
                .tlsEnabled(true)
                .serverId(serverUuid)
                .serverDisplayName("Test Vesta Server")
                .serverDescription("Fixture MOTD")
                .modpackName("Test Pack")
                .modpackVersion("2.4")
                .modpackRevision(12)
                .minimumRevision(9)
                .recommendedMemoryMinMb(4096)
                .recommendedMemoryMaxMb(8192)
                .securityMode(SecurityMode.LINKED)
                .build();

        // Generate persistent TLS keystore before Jetty starts.
        TlsMaterial.ensure(config);

        ServerState state = new ServerState(config);
        AuthService auth = new AuthService(config);
        WhitelistService whitelist = new WhitelistService(root);
        CapabilityEngine capabilities = new CapabilityEngine(
                config.capabilities,
                new PermissionChecker.AllowAll(),
                whitelist::isWhitelisted,
                new OpsLevelProvider.None()
        );

        SecuritySettingsService securitySettings = new SecuritySettingsService(
                configFile,
                auth,
                capabilities,
                9091
        );

        MinecraftSessionChallengeService challenges = new MinecraftSessionChallengeService(
                Duration.ofSeconds(30),
                (name, serverId) -> Optional.empty()
        );

        server = new VestaHttpServer(
                0,
                state,
                config,
                auth,
                new LinkCodeService(60),
                capabilities,
                whitelist,
                challenges,
                new RealtimeTicketService(Duration.ofSeconds(30)),
                new MinecraftServerProcess(config),
                null,
                true,
                securitySettings
        );
        server.start();

        base = URI.create("https://localhost:" + server.boundPort());
        client = trustAllHttpClient();
    }

    @AfterEach
    void stopServer() throws Exception {
        if (server != null) {
            server.stop();
        }
    }

    @Test
    void statusAndDescriptorExposeSha256TlsFingerprint() throws Exception {
        HttpResponse<String> status = client.send(
                HttpRequest.newBuilder(base.resolve(VestaSync.API_STATUS))
                        .timeout(Duration.ofSeconds(3))
                        .GET()
                        .build(),
                HttpResponse.BodyHandlers.ofString()
        );
        assertEquals(200, status.statusCode());

        JsonObject statusJson = JsonParser.parseString(status.body()).getAsJsonObject();
        assertTrue(statusJson.has("tls"));
        assertTrue(statusJson.getAsJsonObject("tls").has("sha256_fingerprint"));
        assertTrue(statusJson.getAsJsonObject("tls").get("sha256_fingerprint").getAsString().matches("(?i)^[0-9a-f]{64}$"));

        HttpResponse<String> descriptor = client.send(
                HttpRequest.newBuilder(base.resolve(VestaSync.API_SERVER_DESCRIPTOR))
                        .timeout(Duration.ofSeconds(3))
                        .GET()
                        .build(),
                HttpResponse.BodyHandlers.ofString()
        );
        assertEquals(200, descriptor.statusCode());

        JsonObject descriptorJson = JsonParser.parseString(descriptor.body()).getAsJsonObject();
        assertTrue(descriptorJson.has("tls"));
        assertEquals(
                statusJson.getAsJsonObject("tls").get("sha256_fingerprint").getAsString(),
                descriptorJson.getAsJsonObject("tls").get("sha256_fingerprint").getAsString()
        );
    }

    private static HttpClient trustAllHttpClient() throws Exception {
        TrustManager[] trustAll = new TrustManager[]{
                new X509TrustManager() {
                    @Override
                    public void checkClientTrusted(X509Certificate[] chain, String authType) {
                    }

                    @Override
                    public void checkServerTrusted(X509Certificate[] chain, String authType) {
                    }

                    @Override
                    public X509Certificate[] getAcceptedIssuers() {
                        return new X509Certificate[0];
                    }
                }
        };

        SSLContext sslContext = SSLContext.getInstance("TLS");
        sslContext.init(null, trustAll, new SecureRandom());

        return HttpClient.newBuilder()
                .sslContext(sslContext)
                .connectTimeout(Duration.ofSeconds(3))
                .build();
    }
}

