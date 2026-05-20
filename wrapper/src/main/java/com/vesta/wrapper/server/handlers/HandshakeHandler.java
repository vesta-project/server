package com.vesta.wrapper.server.handlers;

import com.vesta.sync.protocol.Handshake;
import com.vesta.sync.protocol.HandshakeResponse;
import com.vesta.sync.protocol.TcpMessageCodec;
import com.vesta.sync.VestaSync;
import com.vesta.wrapper.server.ClientConnection;
import com.vesta.wrapper.server.Session;
import com.vesta.wrapper.server.SessionManager;
import org.slf4j.Logger;
import org.slf4j.LoggerFactory;

import java.io.IOException;

public final class HandshakeHandler implements com.vesta.wrapper.server.MessageRouter.MessageHandler {

    private static final Logger log = LoggerFactory.getLogger(HandshakeHandler.class);
    private final SessionManager sessionManager;

    public HandshakeHandler(SessionManager sessionManager) {
        this.sessionManager = sessionManager;
    }

    @Override
    public void handle(ClientConnection client, Object message) throws IOException {
        Handshake hs = (Handshake) message;
        log.info("Handshake from client {} (version: {}, caps: {})", hs.clientId, hs.version, hs.capabilities);

        Session session = sessionManager.create();
        client.setSession(session);

        HandshakeResponse response = new HandshakeResponse();
        response.seq = hs.seq;
        response.ts = TcpMessageCodec.now();
        response.session = session.id;
        response.status = "ok";
        response.serverVersion = VestaSync.API_VERSION;
        response.sessionTtlSeconds = 90;

        client.send(response);
        log.info("Handshake OK, session: {}", session.id);
    }
}
