package com.vesta.loader;

import com.vesta.sync.VestaSync;
import com.vesta.loader.ipc.ControlIpcClient;
import com.vesta.loader.server.ModHttpServer;
import com.vesta.sync.auth.LinkCodeService;

import java.io.IOException;
import java.net.InetSocketAddress;
import java.net.Socket;
import java.nio.file.Path;
import java.util.UUID;

public final class VestaSyncManager {

    private static final VestaSyncManager INSTANCE = new VestaSyncManager();

    private ModHttpServer httpServer;
    private ControlIpcClient controlClient;
    private volatile boolean wrapperMode;
    private final LinkCodeService localLinkCodes = new LinkCodeService(300);

    private VestaSyncManager() {}

    public static VestaSyncManager get() {
        return INSTANCE;
    }

    public void initialize(
            Path serverRoot,
            String modpackName,
            String modpackVersion,
            String mcVersion,
            String loaderType,
            String loaderVersion,
            String host,
            int mcPort
    ) {
        wrapperMode = checkWrapper();
        if (wrapperMode) {
            ModLog.info("Wrapper detected on :{} / :{} — running in wrapper mode (control IPC only)",
                    VestaSync.DEFAULT_CONTROL_PORT, VestaSync.DEFAULT_HTTP_PORT);
            startControlClient();
            return;
        }

        ModLog.info("No wrapper detected — running in standalone mode (embedded ModHttpServer on :{})",
                VestaSync.DEFAULT_HTTP_PORT);
        try {
            httpServer = new ModHttpServer(
                    VestaSync.DEFAULT_HTTP_PORT,
                    serverRoot,
                    modpackName,
                    modpackVersion,
                    mcVersion,
                    loaderType,
                    loaderVersion,
                    host,
                    mcPort
            );
            httpServer.start();
            ModLog.info("Standalone ModHttpServer listening on :{}", VestaSync.DEFAULT_HTTP_PORT);

            startControlClientWatchingForWrapper();
            Runtime.getRuntime().addShutdownHook(new Thread(this::shutdown, "vesta-sync-shutdown"));
        } catch (Exception e) {
            ModLog.error("Failed to start standalone ModHttpServer", e);
        }
    }

    private void startControlClient() {
        controlClient = new ControlIpcClient((uuid, permission) -> {
            if ("vesta.ping".equals(permission)) {
                return true;
            }
            Boolean online = PermissionBridge.check(uuid, permission);
            return online != null && online;
        });
        Thread t = new Thread(controlClient, "vesta-control-client");
        t.setDaemon(true);
        t.start();
        Runtime.getRuntime().addShutdownHook(new Thread(this::shutdown, "vesta-sync-shutdown"));
    }

    private void startControlClientWatchingForWrapper() {
        controlClient = new ControlIpcClient((uuid, permission) -> {
            if ("vesta.ping".equals(permission)) {
                return true;
            }
            Boolean online = PermissionBridge.check(uuid, permission);
            return online != null && online;
        });
        controlClient.setOnConnected(this::onWrapperConnected);
        Thread t = new Thread(controlClient, "vesta-control-client");
        t.setDaemon(true);
        t.start();
    }

    private void onWrapperConnected() {
        if (httpServer == null) {
            wrapperMode = true;
            return;
        }
        ModLog.info("Wrapper control IPC connected — stopping standalone ModHttpServer (switching to wrapper mode)");
        httpServer.stop();
        httpServer = null;
        wrapperMode = true;
    }

    /**
     * Create an in-game link code and notify the wrapper over IPC.
     */
    public String createLinkCode(UUID playerUuid) {
        String code = localLinkCodes.createCode(playerUuid);
        if (controlClient != null) {
            controlClient.sendLinkCode(code, playerUuid);
        }
        return code;
    }

    public void shutdown() {
        if (httpServer != null) {
            httpServer.stop();
            httpServer = null;
        }
        if (controlClient != null) {
            controlClient.stop();
        }
    }

    public boolean isWrapperMode() {
        return wrapperMode;
    }

    /** @deprecated Prefer {@link #isWrapperMode()}. */
    @Deprecated
    public boolean isWrapperDetected() {
        return wrapperMode;
    }

    private static boolean checkWrapper() {
        try (Socket s = new Socket()) {
            s.connect(new InetSocketAddress("127.0.0.1", VestaSync.DEFAULT_CONTROL_PORT), 500);
            return true;
        } catch (IOException e) {
            try (Socket s2 = new Socket()) {
                s2.connect(new InetSocketAddress("127.0.0.1", VestaSync.DEFAULT_HTTP_PORT), 500);
                return true;
            } catch (IOException e2) {
                return false;
            }
        }
    }
}
