package com.vesta.wrapper.server;

import com.vesta.sync.protocol.TcpMessageCodec;
import org.slf4j.Logger;
import org.slf4j.LoggerFactory;

import java.io.IOException;
import java.io.OutputStream;
import java.net.Socket;
import java.util.concurrent.atomic.AtomicLong;

public final class ClientConnection implements Runnable {

    private static final Logger log = LoggerFactory.getLogger(ClientConnection.class);

    private final String id;
    private final Socket socket;
    private final java.io.InputStream inputStream;
    private final MessageRouter router;
    private final AtomicLong seq = new AtomicLong(0);
    private volatile Session session;
    private volatile boolean running = true;
    private volatile long lastActivity;

    public ClientConnection(Socket socket, java.io.InputStream inputStream, MessageRouter router) {
        this.id = socket.getInetAddress().getHostAddress() + ":" + socket.getPort();
        this.socket = socket;
        this.inputStream = inputStream;
        this.router = router;
        this.lastActivity = System.currentTimeMillis();
    }

    public String getId() {
        return id;
    }

    public Session getSession() {
        return session;
    }

    public void setSession(Session session) {
        this.session = session;
    }

    public OutputStream getOutputStream() throws IOException {
        return socket.getOutputStream();
    }

    public long nextSeq() {
        return seq.incrementAndGet();
    }

    public void touch() {
        this.lastActivity = System.currentTimeMillis();
    }

    public void send(Object message) {
        router.send(message, this);
    }

    public boolean isIdle(long timeoutMillis) {
        return System.currentTimeMillis() - lastActivity > timeoutMillis;
    }

    @Override
    public void run() {
        log.info("Client connected: {}", id);
        try {
            socket.setSoTimeout(3_000);
            while (running && !socket.isClosed()) {
                Object message;
                try {
                    message = TcpMessageCodec.decode(inputStream);
                } catch (IOException e) {
                    if (running) {
                        log.debug("Connection read error for {}: {}", id, e.getMessage());
                    }
                    break;
                }

                if (message == null) {
                    break;
                }

                touch();
                router.dispatch(this, message);
            }
        } catch (Exception e) {
            log.error("Unexpected error on connection {}: {}", id, e.getMessage(), e);
        } finally {
            close();
        }
    }

    public void close() {
        if (!running) return;
        running = false;
        try {
            socket.close();
        } catch (IOException ignored) {}
        log.info("Client disconnected: {}", id);
    }
}
