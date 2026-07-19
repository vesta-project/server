package com.vesta.wrapper.server;

import com.vesta.sync.VestaSync;
import com.vesta.wrapper.auth.AuthService;
import com.vesta.wrapper.config.WrapperConfig;
import org.slf4j.Logger;
import org.slf4j.LoggerFactory;

import java.io.IOException;
import java.io.InputStream;
import java.net.ServerSocket;
import java.net.Socket;
import java.net.InetSocketAddress;
import java.util.concurrent.ExecutorService;
import java.util.concurrent.Executors;
import java.util.concurrent.TimeUnit;

/**
 * Legacy plaintext TCP sync transport.
 *
 * <p><strong>Compatibility-only.</strong> New clients should use HTTPS REST and
 * WSS realtime. This path remains for older sync clients and is intentionally
 * not part of the retired-auth cleanup.
 */
public final class TcpSyncServer implements Runnable {

    private static final Logger log = LoggerFactory.getLogger(TcpSyncServer.class);

    private final int port;
    private final boolean multiplexed;
    private final int localHttpPort;
    private final SessionManager sessionManager;
    private final ServerState serverState;
    private final MessageRouter router;
    private final AuthService auth;
    private final ExecutorService workers;
    private volatile boolean running = true;
    private ServerSocket serverSocket;

    public TcpSyncServer(WrapperConfig config, AuthService auth) {
        this.port = config.tcpPort;
        this.multiplexed = (config.tcpPort == config.httpPort);
        this.localHttpPort = config.httpPort + 100;
        this.sessionManager = new SessionManager();
        this.serverState = new ServerState(config);
        this.auth = auth;
        this.router = new MessageRouter(sessionManager, serverState, config, auth);
        this.workers = Executors.newCachedThreadPool(r -> {
            Thread t = new Thread(r, "vesta-tcp-worker");
            t.setDaemon(true);
            return t;
        });
        this.serverState.addPushListener(push ->
                log.info("Push update available: {} — {}", push.newVersion, push.changelog));
    }

    public ServerState getServerState() {
        return serverState;
    }

    public AuthService getAuth() {
        return auth;
    }

    @Override
    public void run() {
        try {
            serverSocket = new ServerSocket();
            serverSocket.bind(new InetSocketAddress(multiplexed ? "127.0.0.1" : "0.0.0.0", port));
            log.info("Vesta Server listening on port {} (Multiplexed: {})", port, multiplexed);
            while (running) {
                try {
                    Socket clientSocket = serverSocket.accept();
                    if (multiplexed) {
                        java.io.PushbackInputStream pin = new java.io.PushbackInputStream(clientSocket.getInputStream(), 1);
                        int firstByte = pin.read();
                        if (firstByte == -1) {
                            clientSocket.close();
                            continue;
                        }

                        if (firstByte == 0) {
                            // Vesta TCP connection — 0x00 marker consumed, frame follows
                            ClientConnection client = new ClientConnection(clientSocket, pin, router);
                            workers.submit(client);
                        } else {
                            // HTTP Connection -> Proxy to local Jetty on localhost:localHttpPort
                            pin.unread(firstByte);
                            workers.submit(new HttpProxyTask(clientSocket, pin, "127.0.0.1", localHttpPort));
                        }
                    } else {
                        // Raw TCP connection
                        ClientConnection client = new ClientConnection(clientSocket, clientSocket.getInputStream(), router);
                        workers.submit(client);
                    }
                } catch (IOException e) {
                    if (running) {
                        log.error("Accept error: {}", e.getMessage());
                    }
                }
            }
        } catch (IOException e) {
            log.error("Server failed to start: {}", e.getMessage());
        } finally {
            shutdown();
        }
    }

    public void stop() {
        running = false;
        try {
            if (serverSocket != null && !serverSocket.isClosed()) {
                serverSocket.close();
            }
        } catch (IOException ignored) {}
        shutdown();
    }

    private void shutdown() {
        workers.shutdown();
        sessionManager.shutdown();
        try {
            if (!workers.awaitTermination(5, TimeUnit.SECONDS)) {
                workers.shutdownNow();
            }
        } catch (InterruptedException e) {
            workers.shutdownNow();
            Thread.currentThread().interrupt();
        }
        log.info("TCP sync server stopped");
    }

    private static class HttpProxyTask implements Runnable {
        private final Socket clientSocket;
        private final InputStream clientIn;
        private final String localHost;
        private final int localPort;

        public HttpProxyTask(Socket clientSocket, InputStream clientIn, String localHost, int localPort) {
            this.clientSocket = clientSocket;
            this.clientIn = clientIn;
            this.localHost = localHost;
            this.localPort = localPort;
        }

        @Override
        public void run() {
            try (Socket localSocket = new Socket(localHost, localPort)) {
                clientSocket.setSoTimeout(30000);
                localSocket.setSoTimeout(30000);

                Thread clientToLocal = new Thread(() -> {
                    try {
                        byte[] buffer = new byte[8192];
                        int len;
                        while ((len = clientIn.read(buffer)) != -1) {
                            localSocket.getOutputStream().write(buffer, 0, len);
                        }
                        localSocket.getOutputStream().flush();
                    } catch (IOException ignored) {
                    } finally {
                        try {
                            localSocket.shutdownOutput();
                        } catch (IOException ignored) {}
                    }
                }, "vesta-proxy-client-to-local");

                clientToLocal.start();

                try {
                    byte[] buffer = new byte[8192];
                    int len;
                    InputStream localIn = localSocket.getInputStream();
                    while ((len = localIn.read(buffer)) != -1) {
                        clientSocket.getOutputStream().write(buffer, 0, len);
                    }
                    clientSocket.getOutputStream().flush();
                } catch (IOException ignored) {
                } finally {
                    try {
                        clientSocket.shutdownOutput();
                    } catch (IOException ignored) {}
                }

                clientToLocal.join(5000);
            } catch (Exception e) {
                log.debug("Proxy task exception: {}", e.getMessage());
            } finally {
                try {
                    clientSocket.close();
                } catch (IOException ignored) {}
            }
        }
    }
}
