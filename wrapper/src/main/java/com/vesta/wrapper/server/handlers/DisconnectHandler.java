package com.vesta.wrapper.server.handlers;

import com.vesta.sync.protocol.Disconnect;
import com.vesta.wrapper.server.ClientConnection;
import com.vesta.wrapper.server.SessionManager;
import org.slf4j.Logger;
import org.slf4j.LoggerFactory;

public final class DisconnectHandler implements com.vesta.wrapper.server.MessageRouter.MessageHandler {

    private static final Logger log = LoggerFactory.getLogger(DisconnectHandler.class);
    private final SessionManager sessionManager;

    public DisconnectHandler(SessionManager sessionManager) {
        this.sessionManager = sessionManager;
    }

    @Override
    public void handle(ClientConnection client, Object message) {
        Disconnect dc = (Disconnect) message;
        log.info("Client disconnecting: session={}, reason={}", dc.session, dc.reason);
        sessionManager.remove(dc.session);
        client.close();
    }
}
