package com.vesta.loader;

import com.vesta.sync.FileStore;
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
    private boolean wrapperDetected;
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
        wrapperDetected = checkWrapper();
        if (wrapperDetected) {
            ModLog.info("Wrapper detected — skipping embedded HTTP; starting control IPC client");
            startControlClient();
            return;
        }

        try {
            Path storeDir = serverRoot.resolve("vesta-store");
            FileStore store = new FileStore(storeDir);
            ModLog.info("File store at {}", store.getRoot());

            httpServer = new ModHttpServer(
                    VestaSync.DEFAULT_HTTP_PORT,
                    store,
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
            ModLog.info("Mod HTTP server on :{}", VestaSync.DEFAULT_HTTP_PORT);

            Runtime.getRuntime().addShutdownHook(new Thread(this::shutdown, "vesta-sync-shutdown"));
        } catch (Exception e) {
            ModLog.error("Failed to start mod HTTP server", e);
        }
    }

    private void startControlClient() {
        controlClient = new ControlIpcClient((uuid, permission) -> {
            // Default deny until a loader wires real LuckPerms/Vault. Still answers IPC.
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
        }
        if (controlClient != null) {
            controlClient.stop();
        }
    }

    public boolean isWrapperDetected() {
        return wrapperDetected;
    }

    private static boolean checkWrapper() {
        try (Socket s = new Socket()) {
            s.connect(new InetSocketAddress("127.0.0.1", VestaSync.DEFAULT_CONTROL_PORT), 500);
            return true;
        } catch (IOException e) {
            // Fall back: anything listening on sync port still counts as wrapper for HTTP skip
            try (Socket s2 = new Socket()) {
                s2.connect(new InetSocketAddress("127.0.0.1", VestaSync.DEFAULT_HTTP_PORT), 500);
                return true;
            } catch (IOException e2) {
                return false;
            }
        }
    }
}
