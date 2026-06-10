package com.vesta.wrapper.server;

import com.vesta.sync.protocol.*;
import com.vesta.sync.util.PublicEndpoint;
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

    public MessageRouter(SessionManager sessionManager, ServerState serverState, WrapperConfig config) {
        this.sessionManager = sessionManager;
        this.serverState = serverState;

        PublicEndpoint endpoint = new PublicEndpoint(
                config.publicScheme,
                config.publicHost,
                config.effectivePublicPort()
        );

        handlers.put(Handshake.class, new HandshakeHandler(sessionManager));
        handlers.put(SyncCheck.class, new SyncCheckHandler(serverState));
        handlers.put(Heartbeat.class, new HeartbeatHandler(sessionManager));
        handlers.put(Disconnect.class, new DisconnectHandler(sessionManager));
        handlers.put(FileRequest.class, new FileRequestHandler(endpoint));
    }

    public void dispatch(ClientConnection client, Object message) {
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

    @FunctionalInterface
    public interface MessageHandler {
        void handle(ClientConnection client, Object message) throws IOException;
    }
}
