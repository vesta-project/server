package com.vesta.wrapper.server;

import com.vesta.sync.VestaSync;
import com.vesta.wrapper.config.WrapperConfig;
import org.slf4j.Logger;
import org.slf4j.LoggerFactory;

import java.io.IOException;
import java.net.ServerSocket;
import java.net.Socket;
import java.util.concurrent.ExecutorService;
import java.util.concurrent.Executors;
import java.util.concurrent.TimeUnit;

public final class TcpSyncServer implements Runnable {

    private static final Logger log = LoggerFactory.getLogger(TcpSyncServer.class);

    private final int port;
    private final SessionManager sessionManager;
    private final ServerState serverState;
    private final MessageRouter router;
    private final ExecutorService workers;
    private volatile boolean running = true;
    private ServerSocket serverSocket;

    public TcpSyncServer(WrapperConfig config) {
        this.port = config.tcpPort;
        this.sessionManager = new SessionManager();
        this.serverState = new ServerState(config);
        this.router = new MessageRouter(sessionManager, serverState);
        this.workers = Executors.newCachedThreadPool(r -> {
            Thread t = new Thread(r, "vesta-tcp-worker");
            t.setDaemon(true);
            return t;
        });
    }

    public ServerState getServerState() {
        return serverState;
    }

    @Override
    public void run() {
        try {
            serverSocket = new ServerSocket(port);
            log.info("TCP sync server listening on port {}", port);
            while (running) {
                try {
                    Socket clientSocket = serverSocket.accept();
                    ClientConnection client = new ClientConnection(clientSocket, router);
                    workers.submit(client);
                } catch (IOException e) {
                    if (running) {
                        log.error("Accept error: {}", e.getMessage());
                    }
                }
            }
        } catch (IOException e) {
            log.error("TCP server failed to start: {}", e.getMessage());
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
}
