package com.vesta.wrapper;

import com.vesta.sync.VestaSync;
import com.vesta.wrapper.server.FileStore;
import com.vesta.wrapper.server.TcpSyncServer;
import com.vesta.wrapper.server.VestaHttpServer;
import org.slf4j.Logger;
import org.slf4j.LoggerFactory;

import java.nio.file.Path;
import java.nio.file.Paths;

public class VestaWrapper {

    private static final Logger log = LoggerFactory.getLogger(VestaWrapper.class);

    public static void main(String[] args) {
        int tcpPort = VestaSync.DEFAULT_TCP_PORT;
        int httpPort = VestaSync.DEFAULT_HTTP_PORT;
        Path fileStore = Paths.get("vesta-files");

        for (int i = 0; i < args.length; i++) {
            switch (args[i]) {
                case "--tcp-port":
                    tcpPort = Integer.parseInt(args[++i]);
                    break;
                case "--http-port":
                    httpPort = Integer.parseInt(args[++i]);
                    break;
                case "--file-store":
                    fileStore = Paths.get(args[++i]);
                    break;
                case "--help":
                case "-h":
                    printUsage();
                    System.exit(0);
            }
        }

        log.info("Vesta Wrapper v{} starting...", VestaSync.API_VERSION);

        FileStore store;
        try {
            store = new FileStore(fileStore);
            log.info("File store at {}", store.getRoot());
        } catch (Exception e) {
            log.error("Failed to create file store: {}", fileStore, e);
            System.exit(1);
            return;
        }

        TcpSyncServer tcpServer = new TcpSyncServer(tcpPort);
        Thread tcpThread = new Thread(tcpServer, "vesta-tcp-server");
        tcpThread.setDaemon(false);
        tcpThread.start();
        log.info("TCP sync server on port {}", tcpPort);

        VestaHttpServer httpServer = new VestaHttpServer(httpPort,
                tcpServer.getServerState(), store);
        try {
            httpServer.start();
            log.info("HTTP REST API on port {}", httpPort);
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
        System.out.println("  --tcp-port <port>    TCP sync port (default: " + VestaSync.DEFAULT_TCP_PORT + ")");
        System.out.println("  --http-port <port>   HTTP API port (default: " + VestaSync.DEFAULT_HTTP_PORT + ")");
        System.out.println("  --file-store <path>  File store directory (default: vesta-files)");
        System.out.println("  --help, -h           Show this help");
    }
}
