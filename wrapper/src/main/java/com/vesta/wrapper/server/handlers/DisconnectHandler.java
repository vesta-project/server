package com.vesta.wrapper.server.handlers;

import com.vesta.sync.protocol.Disconnect;
import com.vesta.wrapper.server.ClientConnection;
import com.vesta.wrapper.server.ServerState;
import com.vesta.wrapper.server.SessionManager;
import com.vesta.wrapper.server.Session;
import org.slf4j.Logger;
import org.slf4j.LoggerFactory;

public final class DisconnectHandler implements com.vesta.wrapper.server.MessageRouter.MessageHandler {

    private static final Logger log = LoggerFactory.getLogger(DisconnectHandler.class);
    private final SessionManager sessionManager;
    private final ServerState serverState;

    public DisconnectHandler(SessionManager sessionManager, ServerState serverState) {
        this.sessionManager = sessionManager;
        this.serverState = serverState;
    }

    @Override
    public void handle(ClientConnection client, Object message) {
        Disconnect dc = (Disconnect) message;
        log.info("Client disconnecting: session={}, reason={}", dc.session, dc.reason);
        Session s = sessionManager.get(dc.session);
        if (s != null && s.packUpdatesListener != null) {
            serverState.removePushListener(s.packUpdatesListener);
        }
        sessionManager.remove(dc.session);
        client.close();
    }
}
