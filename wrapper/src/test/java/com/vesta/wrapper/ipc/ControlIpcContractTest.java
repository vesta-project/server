package com.vesta.wrapper.ipc;

import com.google.gson.JsonObject;
import com.vesta.sync.auth.LinkCodeService;
import com.vesta.sync.ipc.ControlCodec;
import com.vesta.sync.perm.PermissionChecker;
import org.junit.jupiter.api.AfterEach;
import org.junit.jupiter.api.Test;

import java.io.DataInputStream;
import java.io.DataOutputStream;
import java.net.Socket;
import java.util.UUID;

import static org.junit.jupiter.api.Assertions.*;

class ControlIpcContractTest {

    private ControlIpcServer server;

    @AfterEach
    void tearDown() {
        if (server != null) {
            try {
                server.stop();
            } finally {
                server = null;
            }
        }
    }

    @Test
    void heartbeatAndPermissionCheck() throws Exception {
        LinkCodeService links = new LinkCodeService(60);
        server = new ControlIpcServer(0, links);
        Thread serverThread = new Thread(server, "test-control");
        serverThread.setDaemon(true);
        serverThread.start();

        int port;
        long readyDeadline = System.currentTimeMillis() + 5000;
        while ((port = server.boundPort()) <= 0 && System.currentTimeMillis() < readyDeadline) {
            Thread.sleep(50);
        }
        assertTrue(port > 0, "control IPC server should bind to an ephemeral port");

        try (Socket socket = new Socket("127.0.0.1", port)) {
            socket.setTcpNoDelay(true);
            DataOutputStream out = new DataOutputStream(socket.getOutputStream());
            DataInputStream in = new DataInputStream(socket.getInputStream());

            Thread responder = new Thread(() -> {
                try {
                    while (!socket.isClosed()) {
                        JsonObject msg = ControlCodec.read(in);
                        String type = msg.get("type").getAsString();
                        if ("permission_check".equals(type)) {
                            JsonObject result = ControlCodec.permissionResult(true, null);
                            if (msg.has("request_id")) {
                                result.addProperty("request_id", msg.get("request_id").getAsString());
                            }
                            ControlCodec.write(out, result);
                        } else if ("heartbeat_ack".equals(type)) {
                            // ignore
                        }
                    }
                } catch (Exception ignored) {
                }
            }, "fake-mod");
            responder.setDaemon(true);
            responder.start();

            ControlCodec.write(out, ControlCodec.heartbeat());
            long deadline = System.currentTimeMillis() + 5000;
            while (!server.isModConnected() && System.currentTimeMillis() < deadline) {
                ControlCodec.write(out, ControlCodec.heartbeat());
                Thread.sleep(50);
            }
            assertTrue(server.isModConnected(), "mod should be connected after heartbeat");

            PermissionChecker.Result result = server.check(UUID.randomUUID(), "vesta.mods.install");
            assertEquals(PermissionChecker.Result.ALLOW, result);
        }
    }
}
