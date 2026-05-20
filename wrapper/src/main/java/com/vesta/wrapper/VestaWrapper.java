package com.vesta.wrapper;

import com.vesta.sync.VestaSync;
import com.vesta.wrapper.server.TcpSyncServer;
import org.slf4j.Logger;
import org.slf4j.LoggerFactory;

public class VestaWrapper {

    private static final Logger log = LoggerFactory.getLogger(VestaWrapper.class);

    public static void main(String[] args) {
        int tcpPort = VestaSync.DEFAULT_TCP_PORT;

        for (int i = 0; i < args.length; i++) {
            switch (args[i]) {
                case "--port":
                case "-p":
                    tcpPort = Integer.parseInt(args[++i]);
                    break;
                case "--help":
                case "-h":
                    printUsage();
                    System.exit(0);
            }
        }

        log.info("Vesta Wrapper v{} starting...", VestaSync.API_VERSION);
        log.info("TCP sync server on port {}", tcpPort);

        TcpSyncServer tcpServer = new TcpSyncServer(tcpPort);
        Thread tcpThread = new Thread(tcpServer, "vesta-tcp-server");
        tcpThread.setDaemon(false);
        tcpThread.start();

        Runtime.getRuntime().addShutdownHook(new Thread(() -> {
            log.info("Shutting down...");
            tcpServer.stop();
        }));

        log.info("Ready. Press Ctrl+C to stop.");
    }

    private static void printUsage() {
        System.out.println("Vesta Server Sync Wrapper");
        System.out.println("Usage: vesta-wrapper [options]");
        System.out.println("  --port, -p <port>  TCP sync port (default: " + VestaSync.DEFAULT_TCP_PORT + ")");
        System.out.println("  --help, -h         Show this help");
    }
}
