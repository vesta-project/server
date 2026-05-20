package com.vesta.wrapper.server.handlers;

import com.vesta.sync.protocol.Heartbeat;
import com.vesta.wrapper.server.ClientConnection;
import com.vesta.wrapper.server.SessionManager;
import org.slf4j.Logger;
import org.slf4j.LoggerFactory;

public final class HeartbeatHandler implements com.vesta.wrapper.server.MessageRouter.MessageHandler {

    private static final Logger log = LoggerFactory.getLogger(HeartbeatHandler.class);
    private final SessionManager sessionManager;

    public HeartbeatHandler(SessionManager sessionManager) {
        this.sessionManager = sessionManager;
    }

    @Override
    public void handle(ClientConnection client, Object message) {
        Heartbeat hb = (Heartbeat) message;
        sessionManager.touch(hb.session);
        client.touch();

        // echo heartbeat back to client so it knows server is alive
        Heartbeat pong = new Heartbeat();
        pong.seq = client.nextSeq();
        pong.session = hb.session;
        pong.ts = com.vesta.sync.protocol.TcpMessageCodec.now();
        client.send(pong);
    }
}
