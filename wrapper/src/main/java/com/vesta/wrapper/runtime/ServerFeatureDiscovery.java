package com.vesta.wrapper.runtime;

import com.vesta.wrapper.operations.lifecycle.LifecycleBindings;
import com.vesta.wrapper.protocol.ServerFeatureProtocol;
import com.vesta.wrapper.protocol.ServerFeatureProtocol.DiscoveryRequest;
import com.vesta.wrapper.protocol.ServerFeatureProtocol.Feature;
import com.vesta.wrapper.protocol.ServerFeatureProtocol.Manifest;
import com.vesta.wrapper.protocol.ServerFeatureProtocol.Support;

import java.util.List;
import java.util.Objects;

import static com.vesta.wrapper.protocol.ServerFeatureProtocol.FeatureId.*;

/** Trusted in-process reporting only: no network route, authorization decision, or launch preflight. */
public final class ServerFeatureDiscovery {
    private final String serverId;
    private final MinecraftServerProcess game;

    ServerFeatureDiscovery(String canonicalServerId, MinecraftServerProcess game) {
        this.serverId = canonicalServerId;
        this.game = Objects.requireNonNull(game);
    }

    public Manifest discover(DiscoveryRequest request) {
        int version = ServerFeatureProtocol.negotiate(request);
        boolean configured = game.isConfigured();
        return new Manifest(version, serverId, List.of(
                lifecycle(LifecycleBindings.START, configured, "not_configured"),
                lifecycle(LifecycleBindings.STOP, configured && game.gracefulStopSupported(),
                        configured ? "unsupported_stop" : "not_configured"),
                lifecycle(LifecycleBindings.FORCE_KILL, configured, "not_configured"),
                Feature.supported(TASKS_REPLAY),
                Feature.unsupported(GAME_READINESS, "not_implemented"),
                Feature.unsupported(SUPERVISOR_CONTINUITY, "not_implemented")));
    }

    private static Feature lifecycle(String id, boolean supported, String reason) {
        return new Feature(id, supported ? Support.SUPPORTED : Support.UNSUPPORTED, supported ? null : reason);
    }
}
