package com.vesta.wrapper.server;

import com.vesta.sync.VestaSync;
import com.vesta.sync.auth.SecurityMode;
import com.vesta.sync.protocol.Disconnect;
import com.vesta.sync.protocol.Handshake;
import com.vesta.sync.protocol.HandshakeResponse;
import com.vesta.sync.protocol.PushUpdateAvailable;
import com.vesta.sync.protocol.TcpMessageCodec;
import com.vesta.wrapper.auth.AuthService;
import com.vesta.wrapper.config.WrapperConfig;
import com.vesta.wrapper.server.TcpSyncServer;
import org.junit.jupiter.api.AfterEach;
import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.io.TempDir;

import java.io.IOException;
import java.net.ServerSocket;
import java.net.Socket;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.time.Duration;
import java.util.List;
import java.util.UUID;

import static org.junit.jupiter.api.Assertions.*;

class TcpPushUpdateAvailableTest {

    @TempDir
    Path root;

    private TcpSyncServer tcpServer;
    private Thread tcpThread;
    private int tcpPort;

    @AfterEach
    void cleanup() throws Exception {
        if (tcpServer != null) {
            tcpServer.stop();
        }
        if (tcpThread != null) {
            tcpThread.join(2000);
        }
    }

    @Test
    void tcpPushUpdatesAvailableOnRescanWhenClientRequestsPackUpdates() throws Exception {
        tcpPort = findFreePort();
        Path configDir = Files.createDirectories(root.resolve("config"));
        Files.writeString(configDir.resolve("vesta-test.txt"), "tcp push fixture", StandardCharsets.UTF_8);

        UUID serverUuid = UUID.randomUUID();
        WrapperConfig config = new WrapperConfig.Builder()
                .scanRoot(root)
                .scanPaths(List.of("config"))
                .httpPort(0)
                .tcpPort(tcpPort)
                .tlsEnabled(false)
                .serverId(serverUuid)
                .serverDisplayName("Test Vesta Server")
                .modpackName("Test Pack")
                .modpackDescription("Fixture")
                .modpackVersion("2.4")
                .modpackRevision(12)
                .minimumRevision(9)
                .securityMode(SecurityMode.OPEN)
                .build();

        AuthService auth = new AuthService(config);
        tcpServer = new TcpSyncServer(config, auth);
        tcpThread = new Thread(tcpServer, "vesta-tcp-test");
        tcpThread.setDaemon(true);
        tcpThread.start();

        waitForTcpServer("127.0.0.1", tcpPort, Duration.ofSeconds(2));

        try (Socket socket = new Socket("127.0.0.1", tcpPort)) {
            socket.setSoTimeout((int) Duration.ofSeconds(5).toMillis());

            Handshake hs = new Handshake();
            hs.seq = 1;
            hs.ts = TcpMessageCodec.now();
            hs.version = VestaSync.API_VERSION;
            hs.clientId = "tcp-client-test";
            hs.capabilities = List.of("pack_updates");

            socket.getOutputStream().write(TcpMessageCodec.encode(hs));
            socket.getOutputStream().flush();

            Object first = TcpMessageCodec.decode(socket.getInputStream());
            assertNotNull(first, "expected handshake response");
            assertTrue(first instanceof HandshakeResponse, "expected HandshakeResponse but got " + first.getClass());
            HandshakeResponse hr = (HandshakeResponse) first;
            assertEquals("ok", hr.status);
            assertNotNull(hr.session);
            assertFalse(hr.session.isBlank());

            // Register push listener has already been attached in the handshake handler.
            ServerState state = tcpServer.getServerState();
            state.rescan();

            Object pushed = TcpMessageCodec.decode(socket.getInputStream());
            assertNotNull(pushed, "expected push_update_available");
            assertTrue(pushed instanceof PushUpdateAvailable, "expected PushUpdateAvailable but got " + pushed.getClass());
            PushUpdateAvailable push = (PushUpdateAvailable) pushed;

            assertEquals("push_update_available", push.type);
            assertEquals(12, push.revision);
            assertEquals("2.4", push.newVersion);
            assertEquals("2.4", push.previousVersion);
            assertTrue(push.requiresRestart);
            assertFalse(push.forceUpdate);
            assertEquals(hr.session, push.session);

            // Cleanup: send explicit disconnect so the server can remove the per-session push listener.
            Disconnect dc = new Disconnect();
            dc.seq = 2;
            dc.session = hr.session;
            dc.ts = TcpMessageCodec.now();
            dc.reason = "shutdown";
            socket.getOutputStream().write(TcpMessageCodec.encode(dc));
            socket.getOutputStream().flush();
        }
    }

    private static int findFreePort() throws IOException {
        try (ServerSocket s = new ServerSocket(0)) {
            return s.getLocalPort();
        }
    }

    private static void waitForTcpServer(String host, int port, Duration timeout) throws InterruptedException {
        long deadline = System.currentTimeMillis() + timeout.toMillis();
        while (System.currentTimeMillis() < deadline) {
            try (Socket ignored = new Socket(host, port)) {
                return;
            } catch (IOException ignored) {
                Thread.sleep(50);
            }
        }
        fail("Timed out waiting for TCP server on port " + port);
    }
}

