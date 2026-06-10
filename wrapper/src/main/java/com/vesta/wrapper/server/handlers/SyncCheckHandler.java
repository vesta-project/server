package com.vesta.wrapper.server.handlers;

import com.vesta.sync.protocol.SyncCheck;
import com.vesta.sync.protocol.SyncCheckResponse;
import com.vesta.sync.protocol.TcpMessageCodec;
import com.vesta.sync.sync.SyncCheckResult;
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

        SyncCheckResult result = serverState.syncCheck(sc.currentVersion, sc.installedHashes);

        log.info("SyncCheck from session {}: current={}, latest={}, needsUpdate={}, actions={}",
                sc.session, sc.currentVersion, result.newVersion, result.needsUpdate, result.actions.size());

        SyncCheckResponse response = new SyncCheckResponse();
        response.seq = sc.seq;
        response.session = sc.session;
        response.ts = TcpMessageCodec.now();
        response.needsUpdate = result.needsUpdate;
        response.newVersion = result.newVersion;
        response.changelog = result.needsUpdate ? "Modpack update available" : null;
        response.manifest = result.manifest;
        response.actions = result.actions;

        client.send(response);
    }
}
