package com.vesta.wrapper;

import com.vesta.wrapper.config.ConfigLoader;
import com.vesta.wrapper.config.WrapperConfig;
import com.vesta.wrapper.server.FileStore;
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

        FileStore store;
        try {
            store = new FileStore(config.fileStoreDir);
            log.info("  File store at {}", store.getRoot());
        } catch (Exception e) {
            log.error("Failed to create file store: {}", config.fileStoreDir, e);
            System.exit(1);
            return;
        }

        TcpSyncServer tcpServer = new TcpSyncServer(config);
        Thread tcpThread = new Thread(tcpServer, "vesta-tcp-server");
        tcpThread.setDaemon(false);
        tcpThread.start();
        log.info("  TCP sync on :{}", config.tcpPort);

        VestaHttpServer httpServer = new VestaHttpServer(config.httpPort,
                tcpServer.getServerState(), store);
        try {
            httpServer.start();
            log.info("  HTTP REST on :{}", config.httpPort);
        } catch (Exception e) {
            log.error("Failed to start HTTP server", e);
            tcpServer.stop();
            System.exit(1);
        }

        Runtime.getRuntime().addShutdownHook(new Thread(() -> {
            log.info("Shutting down...");
            try { httpServer.stop(); } catch (Exception ignored) {}
            tcpServer.stop();
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
