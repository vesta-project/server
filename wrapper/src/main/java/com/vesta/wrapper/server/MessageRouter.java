package com.vesta.wrapper.server;

import com.vesta.sync.protocol.*;
import com.vesta.sync.util.PublicEndpoint;
import com.vesta.wrapper.auth.AuthService;
import com.vesta.wrapper.config.WrapperConfig;
import com.vesta.wrapper.server.handlers.*;
import org.slf4j.Logger;
import org.slf4j.LoggerFactory;

import java.io.IOException;
import java.util.HashMap;
import java.util.Map;

public final class MessageRouter {

    private static final Logger log = LoggerFactory.getLogger(MessageRouter.class);

    private final Map<Class<?>, MessageHandler> handlers = new HashMap<>();
    private final SessionManager sessionManager;
    private final ServerState serverState;
    private final AuthService auth;

    public MessageRouter(
            SessionManager sessionManager,
            ServerState serverState,
            WrapperConfig config,
            AuthService auth
    ) {
        this.sessionManager = sessionManager;
        this.serverState = serverState;
        this.auth = auth;

        PublicEndpoint endpoint = new PublicEndpoint(
                config.publicScheme,
                config.publicHost,
                config.effectivePublicPort()
        );

        handlers.put(Handshake.class, new HandshakeHandler(sessionManager, serverState, auth));
        handlers.put(SyncCheck.class, new SyncCheckHandler(serverState));
        handlers.put(Heartbeat.class, new HeartbeatHandler(sessionManager));
        handlers.put(Disconnect.class, new DisconnectHandler(sessionManager, serverState));
        handlers.put(FileRequest.class, new FileRequestHandler(endpoint));
    }

    public SessionManager getSessionManager() {
        return sessionManager;
    }

    public AuthService getAuth() {
        return auth;
    }

    public void dispatch(ClientConnection client, Object message) {
        if (!(message instanceof Handshake)) {
            String msgSession = extractSession(message);
            Session local = client.getSession();
            if (local == null || msgSession == null || msgSession.isBlank()) {
                log.warn("Rejecting {}: missing session", message.getClass().getSimpleName());
                client.close();
                return;
            }
            Session lookedUp = sessionManager.get(msgSession);
            if (lookedUp == null || !lookedUp.id.equals(local.id)) {
                log.warn("Rejecting {}: invalid session {}", message.getClass().getSimpleName(), msgSession);
                client.close();
                return;
            }
            sessionManager.touch(msgSession);
            local.touch();
        }

        MessageHandler handler = handlers.get(message.getClass());
        if (handler == null) {
            log.warn("No handler for message type: {}", message.getClass().getSimpleName());
            return;
        }
        try {
            handler.handle(client, message);
        } catch (Exception e) {
            log.error("Error handling message {}: {}", message.getClass().getSimpleName(), e.getMessage(), e);
        }
    }

    public void send(Object message, ClientConnection client) {
        try {
            byte[] frame = TcpMessageCodec.encode(message);
            client.getOutputStream().write(frame);
            client.getOutputStream().flush();
        } catch (IOException e) {
            log.error("Failed to send message to {}: {}", client.getId(), e.getMessage());
            client.close();
        }
    }

    private static String extractSession(Object message) {
        if (message instanceof SyncCheck m) return m.session;
        if (message instanceof FileRequest m) return m.session;
        if (message instanceof Heartbeat m) return m.session;
        if (message instanceof Disconnect m) return m.session;
        if (message instanceof SyncComplete m) return m.session;
        return null;
    }

    @FunctionalInterface
    public interface MessageHandler {
        void handle(ClientConnection client, Object message) throws IOException;
    }
}
