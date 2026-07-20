package com.vesta.wrapper.ipc;

import com.google.gson.JsonObject;
import com.vesta.sync.VestaSync;
import com.vesta.sync.auth.LinkCodeService;
import com.vesta.sync.ipc.ControlCodec;
import com.vesta.sync.perm.PermissionChecker;
import org.slf4j.Logger;
import org.slf4j.LoggerFactory;

import java.io.DataInputStream;
import java.io.DataOutputStream;
import java.io.IOException;
import java.net.InetAddress;
import java.net.ServerSocket;
import java.net.Socket;
import java.net.SocketTimeoutException;
import java.util.UUID;
import java.util.concurrent.CompletableFuture;
import java.util.concurrent.ConcurrentHashMap;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.atomic.AtomicBoolean;
import java.util.concurrent.atomic.AtomicLong;
import java.util.concurrent.atomic.AtomicReference;

/**
 * Loopback control server. One reader thread owns the socket; permission checks use futures.
 */
public final class ControlIpcServer implements Runnable, PermissionChecker {

    private static final Logger log = LoggerFactory.getLogger(ControlIpcServer.class);

    private final int port;
    private final LinkCodeService linkCodes;
    private final AtomicBoolean running = new AtomicBoolean(true);
    private final AtomicBoolean modConnected = new AtomicBoolean(false);
    private final AtomicLong lastHeartbeatMs = new AtomicLong(0);
    private final AtomicReference<Peer> peer = new AtomicReference<>();
    private final ConcurrentHashMap<String, CompletableFuture<Result>> pendingChecks = new ConcurrentHashMap<>();

    private ServerSocket serverSocket;

    public ControlIpcServer(int port, LinkCodeService linkCodes) {
        this.port = port;
        this.linkCodes = linkCodes;
    }

    public boolean isModConnected() {
        long last = lastHeartbeatMs.get();
        return modConnected.get() && last > 0
                && (System.currentTimeMillis() - last) < (VestaSync.HEARTBEAT_TIMEOUT_SEC * 1000L);
    }

    @Override
    public Result check(UUID playerUuid, String permission) {
        Peer p = peer.get();
        if (p == null || !isModConnected()) {
            return Result.UNAVAILABLE;
        }
        String requestId = UUID.randomUUID().toString();
        CompletableFuture<Result> future = new CompletableFuture<>();
        pendingChecks.put(requestId, future);
        try {
            JsonObject req = ControlCodec.permissionCheck(playerUuid.toString(), permission);
            req.addProperty("request_id", requestId);
            synchronized (p.lock) {
                ControlCodec.write(p.out, req);
            }
            return future.get(5, TimeUnit.SECONDS);
        } catch (Exception e) {
            log.warn("Permission IPC failed: {}", e.getMessage());
            return Result.UNAVAILABLE;
        } finally {
            pendingChecks.remove(requestId);
        }
    }

    public LinkCodeService linkCodes() {
        return linkCodes;
    }

    @Override
    public void run() {
        try {
            serverSocket = new ServerSocket(port, 1, InetAddress.getByName("127.0.0.1"));
            log.info("Control IPC listening on 127.0.0.1:{}", port);
            while (running.get()) {
                try {
                    Socket socket = serverSocket.accept();
                    socket.setTcpNoDelay(true);
                    socket.setSoTimeout(VestaSync.HEARTBEAT_TIMEOUT_SEC * 1000);
                    Peer p = new Peer(socket);
                    Peer old = peer.getAndSet(p);
                    if (old != null) {
                        old.close();
                    }
                    modConnected.set(true);
                    lastHeartbeatMs.set(System.currentTimeMillis());
                    log.info("Mod connected to control IPC from {}", socket.getRemoteSocketAddress());
                    Thread t = new Thread(() -> servePeer(p), "vesta-control-peer");
                    t.setDaemon(true);
                    t.start();
                } catch (IOException e) {
                    if (running.get()) {
                        log.debug("Control accept error: {}", e.getMessage());
                    }
                }
            }
        } catch (IOException e) {
            log.error("Failed to bind control IPC on {}: {}", port, e.getMessage());
        }
    }

    private void servePeer(Peer p) {
        try {
            while (running.get() && !p.socket.isClosed()) {
                JsonObject msg;
                try {
                    msg = ControlCodec.read(p.in);
                } catch (SocketTimeoutException e) {
                    continue;
                }
                String type = msg.has("type") ? msg.get("type").getAsString() : "";
                lastHeartbeatMs.set(System.currentTimeMillis());
                switch (type) {
                    case "heartbeat" -> {
                        synchronized (p.lock) {
                            ControlCodec.write(p.out, ControlCodec.heartbeatAck());
                        }
                    }
                    case "link_code" -> {
                        String code = msg.has("code") ? msg.get("code").getAsString() : null;
                        String uuid = msg.has("uuid") ? msg.get("uuid").getAsString() : null;
                        if (code != null && uuid != null) {
                            log.info("Received link code from mod for {}", uuid);
                            linkCodes.noteExternal(code, UUID.fromString(uuid));
                        }
                    }
                    case "permission_result" -> {
                        String requestId = msg.has("request_id") ? msg.get("request_id").getAsString() : null;
                        boolean allowed = msg.has("allowed") && msg.get("allowed").getAsBoolean();
                        Result result = allowed ? Result.ALLOW : Result.DENY;
                        if (requestId != null) {
                            CompletableFuture<Result> f = pendingChecks.remove(requestId);
                            if (f != null) {
                                f.complete(result);
                            }
                        }
                    }
                    default -> log.debug("Unknown control message: {}", type);
                }
            }
        } catch (IOException e) {
            log.info("Mod control peer disconnected: {}", e.getMessage());
        } finally {
            // Only mark the connector offline if this peer is still the active one.
            // The connector opens a short-lived wrapper-detection probe before the
            // persistent control client; closing that probe must not clear the state
            // of the newer, still-connected peer.
            if (peer.compareAndSet(p, null)) {
                modConnected.set(false);
            }
            pendingChecks.forEach((k, f) -> f.complete(Result.UNAVAILABLE));
            pendingChecks.clear();
            p.close();
        }
    }

    public void stop() {
        running.set(false);
        Peer p = peer.getAndSet(null);
        if (p != null) {
            p.close();
        }
        if (serverSocket != null) {
            try {
                serverSocket.close();
            } catch (IOException ignored) {}
        }
    }

    private static final class Peer {
        final Socket socket;
        final DataInputStream in;
        final DataOutputStream out;
        final Object lock = new Object();

        Peer(Socket socket) throws IOException {
            this.socket = socket;
            this.in = new DataInputStream(socket.getInputStream());
            this.out = new DataOutputStream(socket.getOutputStream());
        }

        void close() {
            try {
                socket.close();
            } catch (IOException ignored) {}
        }
    }
}
