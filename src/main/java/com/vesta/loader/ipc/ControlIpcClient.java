package com.vesta.loader.ipc;

import com.google.gson.JsonObject;
import com.vesta.loader.ModLog;
import com.vesta.sync.VestaSync;
import com.vesta.sync.ipc.ControlCodec;

import java.io.DataInputStream;
import java.io.DataOutputStream;
import java.io.IOException;
import java.net.InetSocketAddress;
import java.net.Socket;
import java.util.UUID;
import java.util.concurrent.atomic.AtomicBoolean;
import java.util.function.BiFunction;

/**
 * Connects to the wrapper control IPC on loopback. When wrapper is present, HTTP is skipped
 * but this client stays up for heartbeats, permission checks, and link codes.
 */
public final class ControlIpcClient implements Runnable {

    private final int port;
    private final BiFunction<UUID, String, Boolean> permissionFn;
    private final AtomicBoolean running = new AtomicBoolean(true);
    private volatile Runnable onConnected;
    private volatile boolean connectedNotified;
    private Socket socket;

    public ControlIpcClient(int port, BiFunction<UUID, String, Boolean> permissionFn) {
        this.port = port;
        this.permissionFn = permissionFn;
    }

    public ControlIpcClient(BiFunction<UUID, String, Boolean> permissionFn) {
        this(VestaSync.DEFAULT_CONTROL_PORT, permissionFn);
    }

    public void setOnConnected(Runnable onConnected) {
        this.onConnected = onConnected;
    }

    public void stop() {
        running.set(false);
        if (socket != null) {
            try {
                socket.close();
            } catch (IOException ignored) {}
        }
    }

    public void sendLinkCode(String code, UUID uuid) {
        if (socket == null || socket.isClosed()) {
            return;
        }
        try {
            DataOutputStream out = new DataOutputStream(socket.getOutputStream());
            ControlCodec.write(out, ControlCodec.linkCode(code, uuid.toString()));
        } catch (IOException e) {
            ModLog.warn("Failed to send link code to wrapper: {}", e.getMessage());
        }
    }

    @Override
    public void run() {
        while (running.get()) {
            try (Socket s = new Socket()) {
                s.connect(new InetSocketAddress("127.0.0.1", port), 2000);
                s.setTcpNoDelay(true);
                this.socket = s;
                ModLog.info("Connected to wrapper control IPC on 127.0.0.1:{}", port);
                notifyConnectedOnce();
                DataInputStream in = new DataInputStream(s.getInputStream());
                DataOutputStream out = new DataOutputStream(s.getOutputStream());

                Thread heartbeat = new Thread(() -> {
                    while (running.get() && !s.isClosed()) {
                        try {
                            ControlCodec.write(out, ControlCodec.heartbeat());
                            Thread.sleep(VestaSync.HEARTBEAT_INTERVAL_SEC * 1000L);
                        } catch (Exception e) {
                            break;
                        }
                    }
                }, "vesta-control-heartbeat");
                heartbeat.setDaemon(true);
                heartbeat.start();

                while (running.get() && !s.isClosed()) {
                    JsonObject msg = ControlCodec.read(in);
                    String type = msg.has("type") ? msg.get("type").getAsString() : "";
                    if ("permission_check".equals(type)) {
                        UUID uuid = UUID.fromString(msg.get("uuid").getAsString());
                        String perm = msg.get("permission").getAsString();
                        boolean allowed = Boolean.TRUE.equals(permissionFn.apply(uuid, perm));
                        JsonObject result = ControlCodec.permissionResult(allowed, null);
                        if (msg.has("request_id")) {
                            result.addProperty("request_id", msg.get("request_id").getAsString());
                        }
                        ControlCodec.write(out, result);
                    } else if ("heartbeat_ack".equals(type)) {
                        // ok
                    }
                }
            } catch (Exception e) {
                if (running.get()) {
                    ModLog.warn("Control IPC disconnected, retrying: {}", e.getMessage());
                    try {
                        Thread.sleep(3000);
                    } catch (InterruptedException ie) {
                        Thread.currentThread().interrupt();
                        return;
                    }
                }
            }
        }
    }

    private void notifyConnectedOnce() {
        if (connectedNotified) {
            return;
        }
        connectedNotified = true;
        Runnable callback = onConnected;
        if (callback != null) {
            callback.run();
        }
    }
}
