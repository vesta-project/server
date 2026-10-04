package com.vesta.wrapper;

import com.vesta.sync.VestaSync;
import com.vesta.sync.auth.CapabilityEngine;
import com.vesta.sync.auth.LinkCodeService;
import com.vesta.sync.auth.MinecraftSessionChallengeService;
import com.vesta.sync.auth.MsaProfileVerifier;
import com.vesta.sync.auth.OpsJsonReader;
import com.vesta.sync.auth.RealtimeTicketService;
import com.vesta.wrapper.auth.AuthService;
import com.vesta.wrapper.config.ConfigLoader;
import com.vesta.wrapper.config.SecuritySettingsService;
import com.vesta.wrapper.config.WrapperConfig;
import com.vesta.wrapper.ipc.ControlIpcServer;
import com.vesta.wrapper.runtime.AgentRuntime;
import com.vesta.wrapper.runtime.MinecraftServerProcess;
import com.vesta.wrapper.security.WhitelistService;
import com.vesta.wrapper.security.TlsMaterial;
import com.vesta.wrapper.server.TcpSyncServer;
import com.vesta.wrapper.server.VestaHttpServer;
import org.slf4j.Logger;
import org.slf4j.LoggerFactory;

import java.nio.file.Path;
import java.time.Duration;

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

        WrapperConfig config;
        try {
            config = ConfigLoader.load(configFile);
        } catch (ConfigLoader.ConfigException error) {
            // Fail closed: never open legacy listeners with permissive defaults for a broken config.
            log.error("Refusing to start: {}", error.getMessage());
            System.exit(1);
            return;
        }
        MinecraftServerProcess minecraftServer = new MinecraftServerProcess(config);
        log.info("Vesta Wrapper — {}", config.modpackName);
        log.info("  Version {} for Minecraft {} ({} {})",
                config.modpackVersion, config.minecraftVersion,
                config.modloaderType, config.modloaderVersion);
        if (config.tlsEnabled && config.tcpPort == config.httpPort) {
            log.error("TLS requires separate ports. Keep HTTPS on :{} and move raw TCP compatibility to another port.", config.httpPort);
            return;
        }
        if (config.tlsEnabled) {
            try {
                TlsMaterial.ensure(config);
            } catch (Exception error) {
                log.error("Could not prepare the HTTPS identity", error);
                return;
            }
        }

        // Durable runtime (opt-in) opens BEFORE any listener and before any Minecraft launch, so the runner
        // lock, startup reconciliation and generation tracking are in place first. Failure is fatal: we never
        // fall back to legacy lifecycle routes when durable mode was requested.
        // Default (disabled) legacy mode is a compatibility checkpoint, NOT the secure product.
        AgentRuntime agentRuntime = null;
        if (config.durableRuntimeEnabled) {
            try {
                agentRuntime = AgentRuntime.open(config, minecraftServer);
                log.info("  Durable runtime state: {}", agentRuntime.stateDirectory().path());
            } catch (RuntimeException error) {
                log.error("Could not open the durable agent runtime; refusing to start", error);
                System.exit(1);
                return;
            }
        } else {
            log.warn("  Durable runtime disabled ([state] enabled = false): legacy unaudited lifecycle routes are active");
        }
        final AgentRuntime runtime = agentRuntime;

        AuthService auth = new AuthService(config);
        LinkCodeService linkCodes = new LinkCodeService(300);
        MsaProfileVerifier sessionVerifier = new MsaProfileVerifier();
        MinecraftSessionChallengeService minecraftChallenges =
                new MinecraftSessionChallengeService(
                        Duration.ofSeconds(60),
                        (username, serverId) -> sessionVerifier
                                .verifyJoined(username, serverId)
                                .map(profile -> new MinecraftSessionChallengeService.VerifiedProfile(
                                        profile.uuid,
                                        profile.name
                                ))
                );
        RealtimeTicketService realtimeTickets = new RealtimeTicketService(Duration.ofSeconds(30));
        ControlIpcServer controlIpc = new ControlIpcServer(
                config.controlPort > 0 ? config.controlPort : VestaSync.DEFAULT_CONTROL_PORT,
                linkCodes
        );
        Thread controlThread = new Thread(controlIpc, "vesta-control-ipc");
        controlThread.setDaemon(true);
        controlThread.start();

        WhitelistService whitelist = new WhitelistService(config.scanRoot);

        TcpSyncServer tcpServer = new TcpSyncServer(config, auth);
        Thread tcpThread = new Thread(tcpServer, "vesta-tcp-server");
        tcpThread.setDaemon(false);
        tcpThread.start();
        log.info("  TCP sync on :{}", config.tcpPort);

        CapabilityEngine capabilities = new CapabilityEngine(
                config.capabilities,
                controlIpc,
                whitelist::isWhitelisted,
                new OpsJsonReader(config.scanRoot)
        );
        SecuritySettingsService securitySettings = new SecuritySettingsService(
                configFile.toAbsolutePath().normalize(),
                auth,
                capabilities,
                config.controlPort > 0 ? config.controlPort : VestaSync.DEFAULT_CONTROL_PORT
        );

        boolean multiplexed = !config.tlsEnabled && (config.tcpPort == config.httpPort);
        int actualHttpPort = multiplexed ? (config.httpPort + 100) : config.httpPort;

        VestaHttpServer httpServer = new VestaHttpServer(
                actualHttpPort,
                tcpServer.getServerState(),
                config,
                auth,
                linkCodes,
                capabilities,
                whitelist,
                minecraftChallenges,
                realtimeTickets,
                minecraftServer,
                controlIpc,
                multiplexed,
                securitySettings,
                runtime != null
        );
        try {
            httpServer.start();
            if (multiplexed) {
                log.info("  HTTP REST internally on :{} (Multiplexed on public port :{})", actualHttpPort, config.httpPort);
            } else {
                log.info("  {} REST/WSS on :{}", config.tlsEnabled ? "HTTPS" : "HTTP (local only)", config.httpPort);
            }
            log.info("  Control IPC on 127.0.0.1:{}", config.controlPort > 0 ? config.controlPort : VestaSync.DEFAULT_CONTROL_PORT);
            log.info("  Devtools UI: {}://localhost:{}/devtools", config.tlsEnabled ? "https" : "http", config.httpPort);
            // Legacy auto-start: outside operation scope (not an audited operation, no actor). It runs only
            // after the durable runtime opened, and the launch still gets a generation id so typed stop/kill
            // can target it.
            minecraftServer.startIfConfigured();
        } catch (Exception e) {
            log.error("Failed to start Vesta or the configured Minecraft server", e);
            try { httpServer.stop(); } catch (Exception ignored) {}
            tcpServer.stop();
            controlIpc.stop();
            if (runtime != null) {
                runtime.shutdown();
            }
            System.exit(1);
        }

        Runtime.getRuntime().addShutdownHook(new Thread(() -> {
            log.info("Shutting down...");
            try { httpServer.stop(); } catch (Exception ignored) {}
            tcpServer.stop();
            controlIpc.stop();
            // Legacy game stop only after the runtime fully stopped: never race a still-active lifecycle
            // handler (which may hold the game.generation lock) with a second, unlocked stop.
            if (runtime != null) {
                runtime.shutdown();
            }
            if (runtime != null && !runtime.runner().fullyStopped()) {
                log.warn("Durable runtime did not stop within its bounded grace; lifecycle handlers still own "
                        + "the game and state. Skipping the legacy game stop; the game process is left to the "
                        + "running handler and JVM exit");
                return;
            }
            minecraftServer.stop();
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
