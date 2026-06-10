package com.vesta.loader;

import com.vesta.sync.FileStore;
import com.vesta.sync.VestaSync;
import com.vesta.loader.server.ModHttpServer;

import java.io.IOException;
import java.net.InetSocketAddress;
import java.net.Socket;
import java.nio.file.Path;

public final class VestaSyncManager {

    private static final VestaSyncManager INSTANCE = new VestaSyncManager();

    private ModHttpServer httpServer;
    private boolean wrapperDetected;

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
            ModLog.info("Wrapper detected, skipping embedded HTTP server");
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

    public void shutdown() {
        if (httpServer != null) {
            httpServer.stop();
        }
    }

    public boolean isWrapperDetected() {
        return wrapperDetected;
    }

    private static boolean checkWrapper() {
        try (Socket s = new Socket()) {
            s.connect(new InetSocketAddress("127.0.0.1", VestaSync.DEFAULT_HTTP_PORT), 500);
            return true;
        } catch (IOException e) {
            return false;
        }
    }
}
