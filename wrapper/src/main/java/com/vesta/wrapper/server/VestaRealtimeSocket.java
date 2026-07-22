package com.vesta.wrapper.server;

import com.google.gson.Gson;
import com.google.gson.JsonObject;
import com.vesta.sync.auth.BearerTokenStore;
import com.vesta.sync.auth.CapabilityEngine;
import com.vesta.sync.auth.RealtimeTicketService;
import com.vesta.sync.protocol.PushUpdateAvailable;
import com.vesta.wrapper.auth.AuthService;
import com.vesta.wrapper.console.ConsoleBuffer;
import org.eclipse.jetty.websocket.api.Callback;
import org.eclipse.jetty.websocket.api.Session;
import org.eclipse.jetty.websocket.api.StatusCode;
import org.eclipse.jetty.websocket.api.annotations.OnWebSocketClose;
import org.eclipse.jetty.websocket.api.annotations.OnWebSocketError;
import org.eclipse.jetty.websocket.api.annotations.OnWebSocketMessage;
import org.eclipse.jetty.websocket.api.annotations.OnWebSocketOpen;
import org.eclipse.jetty.websocket.api.annotations.WebSocket;

import java.time.Instant;
import java.util.LinkedHashMap;
import java.util.Map;
import java.util.Optional;
import java.util.UUID;
import java.util.concurrent.CompletableFuture;
import java.util.concurrent.TimeUnit;
import java.util.function.Consumer;

@WebSocket
public final class VestaRealtimeSocket {
    private static final Gson GSON = new Gson();
    private static final int MAX_MESSAGES_PER_WINDOW = 30;

    private final UUID serverUuid;
    private final RealtimeTicketService tickets;
    private final ServerState serverState;
    private final AuthService auth;
    private final CapabilityEngine capabilities;
    private final ConsoleBuffer consoleBuffer;
    private final Consumer<PushUpdateAvailable> pushListener;
    private final Consumer<ConsoleBuffer.Line> consoleListener;
    private Session session;
    private volatile RealtimeTicketService.Ticket principal;
    private long rateWindowStarted = System.currentTimeMillis();
    private int rateWindowMessages;

    public VestaRealtimeSocket(
            UUID serverUuid,
            RealtimeTicketService tickets,
            ServerState serverState,
            AuthService auth,
            CapabilityEngine capabilities,
            ConsoleBuffer consoleBuffer
    ) {
        this.serverUuid = serverUuid;
        this.tickets = tickets;
        this.serverState = serverState;
        this.auth = auth;
        this.capabilities = capabilities;
        this.consoleBuffer = consoleBuffer;
        this.pushListener = this::onPackUpdate;
        this.consoleListener = this::onConsoleLine;
    }

    @OnWebSocketOpen
    public void onOpen(Session session) {
        this.session = session;
        CompletableFuture.delayedExecutor(10, TimeUnit.SECONDS).execute(() -> {
            if (this.session != null && this.session.isOpen() && principal == null) {
                close(StatusCode.POLICY_VIOLATION, "authentication timeout");
            }
        });
    }

    @OnWebSocketMessage
    public void onMessage(String message) {
        if (!allowMessage()) {
            close(StatusCode.POLICY_VIOLATION, "rate limit exceeded");
            return;
        }
        JsonObject body;
        try {
            body = GSON.fromJson(message, JsonObject.class);
        } catch (Exception error) {
            close(StatusCode.BAD_DATA, "invalid json");
            return;
        }
        String type = body != null && body.has("type") ? body.get("type").getAsString() : "";
        if (principal == null) {
            if (!"authenticate".equals(type) || !body.has("ticket")) {
                close(StatusCode.POLICY_VIOLATION, "authenticate first");
                return;
            }
            Optional<RealtimeTicketService.Ticket> consumed =
                    tickets.consume(serverUuid, body.get("ticket").getAsString());
            if (consumed.isEmpty()) {
                close(StatusCode.POLICY_VIOLATION, "invalid or expired ticket");
                return;
            }
            principal = consumed.get();
            serverState.addPushListener(pushListener);
            if (principal.capabilities.contains("console")) {
                consoleBuffer.addListener(consoleListener);
            }
            send(Map.of(
                    "type", "ready",
                    "server_id", serverUuid.toString(),
                    "capabilities", principal.capabilities,
                    "expires_at", principal.sessionExpiresAt.toString()
            ));
            long delay = Math.max(0, principal.sessionExpiresAt.toEpochMilli() - System.currentTimeMillis());
            CompletableFuture.delayedExecutor(delay, TimeUnit.MILLISECONDS).execute(() -> {
                if (session.isOpen()) close(StatusCode.POLICY_VIOLATION, "session expired");
            });
            return;
        }
        if (!Instant.now().isBefore(principal.sessionExpiresAt)) {
            close(StatusCode.POLICY_VIOLATION, "session expired");
            return;
        }
        if (!hasCurrentAccess()) {
            return;
        }
        if ("ping".equals(type)) {
            send(Map.of("type", "pong", "at", Instant.now().toString()));
        } else if ("status".equals(type)) {
            Map<String, Object> event = new LinkedHashMap<>();
            event.put("type", "server_status");
            event.put("revision", serverState.getCurrentRevision());
            event.put("minimum_revision", serverState.getMinimumRevision());
            event.put("display_version", serverState.getCurrentVersion());
            event.put("updated_at", serverState.getUpdatedAt().toString());
            send(event);
        } else {
            send(Map.of("type", "error", "error", "unsupported message"));
        }
    }

    @OnWebSocketClose
    public void onClose(int statusCode, String reason) {
        serverState.removePushListener(pushListener);
        consoleBuffer.removeListener(consoleListener);
        session = null;
        principal = null;
    }

    @OnWebSocketError
    public void onError(Throwable error) {
        if (session != null && session.isOpen()) {
            close(StatusCode.SERVER_ERROR, "realtime error");
        }
    }

    private boolean allowMessage() {
        long now = System.currentTimeMillis();
        if (now - rateWindowStarted >= 1_000) {
            rateWindowStarted = now;
            rateWindowMessages = 0;
        }
        return ++rateWindowMessages <= MAX_MESSAGES_PER_WINDOW;
    }

    private boolean hasCurrentAccess() {
        RealtimeTicketService.Ticket current = principal;
        Optional<BearerTokenStore.Record> bearer = auth.store().validateHash(current.bearerTokenHash);
        if (bearer.isEmpty()
                || !current.accountUuid.equals(bearer.get().boundUuid)
                || !current.role.equals(bearer.get().role)) {
            accessChanged("credential revoked or expired");
            return false;
        }
        CapabilityEngine.AuthContext context = new CapabilityEngine.AuthContext(
                auth.mode(),
                true,
                "operator".equals(current.role),
                current.accountUuid,
                current.role
        );
        CapabilityEngine.Decision decision = capabilities.check(
                CapabilityEngine.Action.PUBLIC_MODPACK,
                context
        );
        if (decision != CapabilityEngine.Decision.ALLOW) {
            accessChanged(decision.name().toLowerCase());
            return false;
        }
        return true;
    }

    private void accessChanged(String reason) {
        send(Map.of(
                "type", "access_changed",
                "access_state", "access_required",
                "reason", reason
        ));
        close(StatusCode.POLICY_VIOLATION, "access changed");
    }

    private void onPackUpdate(PushUpdateAvailable ignored) {
        RealtimeTicketService.Ticket current = principal;
        if (current == null || !current.capabilities.contains("pack_updates")) {
            return;
        }
        Map<String, Object> event = new LinkedHashMap<>();
        event.put("type", "pack_update");
        event.put("revision", serverState.getCurrentRevision());
        event.put("minimum_revision", serverState.getMinimumRevision());
        event.put("display_version", serverState.getCurrentVersion());
        event.put("updated_at", serverState.getUpdatedAt().toString());
        send(event);
    }

    private void onConsoleLine(ConsoleBuffer.Line line) {
        RealtimeTicketService.Ticket current = principal;
        if (current == null || !current.capabilities.contains("console")) {
            return;
        }
        if (!allowMessage()) {
            return;
        }
        Map<String, Object> event = new LinkedHashMap<>();
        event.put("type", "console_line");
        event.put("cursor", line.cursor);
        event.put("ts", line.ts.toString());
        event.put("stream", line.stream);
        event.put("text", line.text);
        send(event);
    }

    private void send(Object value) {
        if (session != null && session.isOpen()) {
            session.sendText(GSON.toJson(value), Callback.NOOP);
        }
    }

    private void close(int status, String reason) {
        if (session != null && session.isOpen()) {
            session.close(status, reason, Callback.NOOP);
        }
    }
}
