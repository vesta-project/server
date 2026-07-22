package com.vesta.wrapper.server.handlers;

import com.vesta.sync.protocol.Disconnect;
import com.vesta.wrapper.server.ClientConnection;
import org.slf4j.Logger;
import org.slf4j.LoggerFactory;

public final class DisconnectHandler implements com.vesta.wrapper.server.MessageRouter.MessageHandler {

    private static final Logger log = LoggerFactory.getLogger(DisconnectHandler.class);

    @Override
    public void handle(ClientConnection client, Object message) {
        Disconnect dc = (Disconnect) message;
        log.info("Client disconnecting: session={}, reason={}", dc.session, dc.reason);
        // ClientConnection.close() unregisters push listeners and removes the session.
        client.close();
    }
}
