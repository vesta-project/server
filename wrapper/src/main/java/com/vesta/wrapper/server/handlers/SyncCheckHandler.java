package com.vesta.wrapper.server.handlers;

import com.vesta.sync.protocol.SyncCheck;
import com.vesta.sync.protocol.SyncCheckResponse;
import com.vesta.sync.protocol.TcpMessageCodec;
import com.vesta.wrapper.server.ClientConnection;
import com.vesta.wrapper.server.ServerState;
import org.slf4j.Logger;
import org.slf4j.LoggerFactory;

import java.io.IOException;

public final class SyncCheckHandler implements com.vesta.wrapper.server.MessageRouter.MessageHandler {

    private static final Logger log = LoggerFactory.getLogger(SyncCheckHandler.class);
    private final ServerState serverState;

    public SyncCheckHandler(ServerState serverState) {
        this.serverState = serverState;
    }

    @Override
    public void handle(ClientConnection client, Object message) throws IOException {
        SyncCheck sc = (SyncCheck) message;

        if (client.getSession() != null) {
            client.getSession().currentVersion = sc.currentVersion;
        }

        String latestVersion = serverState.getCurrentVersion();
        boolean needsUpdate = !latestVersion.equals(sc.currentVersion);

        log.info("SyncCheck from session {}: current={}, latest={}, needsUpdate={}",
                sc.session, sc.currentVersion, latestVersion, needsUpdate);

        SyncCheckResponse response = new SyncCheckResponse();
        response.seq = sc.seq;
        response.session = sc.session;
        response.ts = TcpMessageCodec.now();
        response.needsUpdate = needsUpdate;
        response.newVersion = latestVersion;
        response.changelog = "Demo changelog: initial sync";

        if (needsUpdate) {
            response.manifest = serverState.getManifest();
        }

        client.send(response);
    }
}
