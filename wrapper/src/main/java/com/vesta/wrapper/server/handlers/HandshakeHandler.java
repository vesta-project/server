package com.vesta.wrapper.server.handlers;

import com.vesta.sync.VestaSync;
import com.vesta.sync.auth.AuthGate;
import com.vesta.sync.auth.BearerTokenStore;
import com.vesta.sync.auth.TokenService;
import com.vesta.sync.protocol.Handshake;
import com.vesta.sync.protocol.HandshakeResponse;
import com.vesta.sync.protocol.PushUpdateAvailable;
import com.vesta.sync.protocol.TcpMessageCodec;
import com.vesta.wrapper.auth.AuthService;
import com.vesta.wrapper.server.ClientConnection;
import com.vesta.wrapper.server.ServerState;
import com.vesta.wrapper.server.Session;
import com.vesta.wrapper.server.SessionManager;
import org.slf4j.Logger;
import org.slf4j.LoggerFactory;

import java.io.IOException;
import java.util.HashSet;
import java.util.Optional;
import java.util.function.Consumer;

public final class HandshakeHandler implements com.vesta.wrapper.server.MessageRouter.MessageHandler {

    private static final Logger log = LoggerFactory.getLogger(HandshakeHandler.class);
    private final SessionManager sessionManager;
    private final AuthService auth;
    private final ServerState serverState;

    public HandshakeHandler(SessionManager sessionManager, ServerState serverState, AuthService auth) {
        this.sessionManager = sessionManager;
        this.serverState = serverState;
        this.auth = auth;
    }

    private void maybeRegisterPushListener(
            ClientConnection client,
            Session session,
            Handshake hs
    ) {
        // TCP clients can announce feature scopes in the handshake. We use the same
        // "pack_updates" gate as the websocket path.
        session.capabilities = hs.capabilities != null
                ? new HashSet<>(hs.capabilities)
                : java.util.Collections.emptySet();

        if (!session.capabilities.contains("pack_updates")) {
            return;
        }

        Consumer<PushUpdateAvailable> listener = push -> {
            // Create a per-session message instance so we can set session without
            // mutating a shared object across multiple listeners.
            PushUpdateAvailable msg = new PushUpdateAvailable();
            msg.seq = push.seq;
            msg.session = session.id;
            msg.ts = push.ts;
            msg.revision = push.revision;
            msg.newVersion = push.newVersion;
            msg.previousVersion = push.previousVersion;
            msg.changelog = push.changelog;
            msg.requiresRestart = push.requiresRestart;
            msg.forceUpdate = push.forceUpdate;
            client.send(msg);
        };

        session.packUpdatesListener = listener;
        serverState.addPushListener(listener);
    }

    @Override
    public void handle(ClientConnection client, Object message) throws IOException {
        Handshake hs = (Handshake) message;
        log.info("Handshake from client {} (version: {}, caps: {})", hs.clientId, hs.version, hs.capabilities);

        HandshakeResponse response = new HandshakeResponse();
        response.seq = hs.seq;
        response.ts = TcpMessageCodec.now();
        response.serverVersion = VestaSync.API_VERSION;
        response.sessionTtlSeconds = 90;

        if (!auth.isOpen()) {
            boolean ok = false;
            if (hs.authToken != null && !hs.authToken.isBlank()) {
                Optional<BearerTokenStore.Record> rec = auth.validateBearer(hs.authToken);
                ok = rec.isPresent();
                if (ok) {
                    Session session = sessionManager.create();
                    session.bearerTokenHash = TokenService.hashToken(AuthGate.extractBearer(hs.authToken));
                    session.role = rec.get().role;
                    client.setSession(session);
                    maybeRegisterPushListener(client, session, hs);
                    response.session = session.id;
                    response.status = "ok";
                    client.send(response);
                    log.info("Handshake OK (bearer), session: {}", session.id);
                    return;
                }
            }
            if (hs.password != null && !hs.password.isBlank()) {
                Optional<String> issued = auth.login(hs.password);
                if (issued.isPresent()) {
                    Session session = sessionManager.create();
                    session.bearerTokenHash = TokenService.hashToken(issued.get());
                    session.role = "operator";
                    client.setSession(session);
                    maybeRegisterPushListener(client, session, hs);
                    response.session = session.id;
                    response.status = "ok";
                    // Clients that need the bearer for HTTP can request /api/v1/auth/token separately.
                    client.send(response);
                    log.info("Handshake OK (password), session: {}", session.id);
                    return;
                }
                response.status = "error";
                response.errorMessage = "invalid password";
                response.session = "";
                client.send(response);
                log.warn("Handshake rejected for {}: invalid password", hs.clientId);
                return;
            }
            response.status = "error";
            response.errorMessage = "authentication required";
            response.session = "";
            client.send(response);
            log.warn("Handshake rejected for {}: missing credentials", hs.clientId);
            return;
        }

        Session session = sessionManager.create();
        session.role = "anonymous";
        client.setSession(session);
        maybeRegisterPushListener(client, session, hs);
        response.session = session.id;
        response.status = "ok";
        client.send(response);
        log.info("Handshake OK (open), session: {}", session.id);
    }
}
