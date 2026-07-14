package com.vesta.wrapper;

import com.vesta.sync.FileStore;
import com.vesta.sync.VestaSync;
import com.vesta.sync.auth.LinkCodeService;
import com.vesta.wrapper.auth.AuthService;
import com.vesta.wrapper.config.ConfigLoader;
import com.vesta.wrapper.config.WrapperConfig;
import com.vesta.wrapper.ipc.ControlIpcServer;
import com.vesta.wrapper.server.TcpSyncServer;
import com.vesta.wrapper.server.VestaHttpServer;
import org.slf4j.Logger;
import org.slf4j.LoggerFactory;

import java.nio.file.Path;

public class VestaWrapper {

    private static final Logger log = LoggerFactory.getLogger(VestaWrapper.class);

    public static void main(String[] args) {
        Path configFile = Path.of("wrapper-config.toml");
        for (int i = 0; i < args.length; i++) {
            switch (args[i]) {
                case "--config":
                case "-c":
                    configFile = Path.of(args[++i]);
                    break;
                case "--help":
                case "-h":
                    printUsage();
                    System.exit(0);
            }
        }

        WrapperConfig config = ConfigLoader.load(configFile);
        log.info("Vesta Wrapper — {}", config.modpackName);
        log.info("  Version {} for Minecraft {} ({} {})",
                config.modpackVersion, config.minecraftVersion,
                config.modloaderType, config.modloaderVersion);

        AuthService auth = new AuthService(config);
        LinkCodeService linkCodes = new LinkCodeService(300);
        ControlIpcServer controlIpc = new ControlIpcServer(
                config.controlPort > 0 ? config.controlPort : VestaSync.DEFAULT_CONTROL_PORT,
                linkCodes
        );
        Thread controlThread = new Thread(controlIpc, "vesta-control-ipc");
        controlThread.setDaemon(true);
        controlThread.start();

        FileStore store;
        try {
            store = new FileStore(config.fileStoreDir);
            log.info("  File store at {}", store.getRoot());
        } catch (Exception e) {
            log.error("Failed to create file store: {}", config.fileStoreDir, e);
            System.exit(1);
            return;
        }

        TcpSyncServer tcpServer = new TcpSyncServer(config, store, auth);
        Thread tcpThread = new Thread(tcpServer, "vesta-tcp-server");
        tcpThread.setDaemon(false);
        tcpThread.start();
        log.info("  TCP sync on :{}", config.tcpPort);

        boolean multiplexed = (config.tcpPort == config.httpPort);
        int actualHttpPort = multiplexed ? (config.httpPort + 100) : config.httpPort;

        VestaHttpServer httpServer = new VestaHttpServer(
                actualHttpPort,
                tcpServer.getServerState(),
                store,
                auth,
                linkCodes,
                controlIpc,
                multiplexed
        );
        try {
            httpServer.start();
            if (multiplexed) {
                log.info("  HTTP REST internally on :{} (Multiplexed on public port :{})", actualHttpPort, config.httpPort);
            } else {
                log.info("  HTTP REST on :{}", config.httpPort);
            }
            log.info("  Control IPC on 127.0.0.1:{}", config.controlPort > 0 ? config.controlPort : VestaSync.DEFAULT_CONTROL_PORT);
        } catch (Exception e) {
            log.error("Failed to start HTTP server", e);
            tcpServer.stop();
            controlIpc.stop();
            System.exit(1);
        }

        Runtime.getRuntime().addShutdownHook(new Thread(() -> {
            log.info("Shutting down...");
            try { httpServer.stop(); } catch (Exception ignored) {}
            tcpServer.stop();
            controlIpc.stop();
        }));

        log.info("Ready. Press Ctrl+C to stop.");
    }

    private static void printUsage() {
        System.out.println("Vesta Server Sync Wrapper");
        System.out.println("Usage: vesta-wrapper [options]");
        System.out.println("  --config, -c <file>  Config file (default: wrapper-config.toml)");
        System.out.println("  --help, -h           Show this help");
        System.out.println();
        System.out.println("All runtime options are configured in the config file.");
        System.out.println("See wrapper-config.toml for available settings.");
    }
}
