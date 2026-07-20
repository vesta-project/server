package com.vesta.loader.platform;

import org.bukkit.Bukkit;
import org.bukkit.plugin.java.JavaPlugin;
import com.vesta.loader.VestaMod;

import java.nio.file.Path;

public class PaperEntrypoint extends JavaPlugin {
    private VestaMod mod;

    @Override
    public void onEnable() {
        mod = new VestaMod();
        mod.onInitialize();

        Path gameDir = getDataFolder().toPath().toAbsolutePath();
        if (gameDir.getParent() != null && gameDir.getParent().getParent() != null) {
            gameDir = gameDir.getParent().getParent();
        } else {
            gameDir = Path.of(System.getProperty("user.dir"));
        }

        String mcVersion = "unknown";
        try {
            mcVersion = Bukkit.getMinecraftVersion();
        } catch (Throwable t) {
            String version = Bukkit.getVersion();
            int start = version.indexOf("MC: ");
            if (start != -1) {
                int end = version.indexOf(")", start);
                if (end != -1) {
                    mcVersion = version.substring(start + 4, end);
                }
            }
        }

        mod.onServerStart(
            gameDir,
            "Server Modpack",
            "1.0.0",
            mcVersion,
            "paper",
            "latest",
            Bukkit.getIp().isEmpty() ? "0.0.0.0" : Bukkit.getIp(),
            Bukkit.getPort()
        );

        com.vesta.loader.PermissionBridge.setDelegate((uuid, permission) -> {
            org.bukkit.OfflinePlayer player = Bukkit.getOfflinePlayer(uuid);
            if ("vesta.ping".equals(permission)) {
                return true;
            }
            if ("vesta.op".equals(permission)) {
                return player.isOp();
            }
            if (player.isOp() && permission.startsWith("vesta.")) {
                return true;
            }
            // OfflinePlayer has no permission resolver; only online players can be
            // checked against plugin permissions (e.g. LuckPerms). Offline players
            // fall back to the OP-based checks above.
            org.bukkit.entity.Player online = Bukkit.getPlayer(uuid);
            if (online != null) {
                return online.hasPermission(permission);
            }
            return false;
        });

        // Paper plugins (paper-plugin.yml) cannot declare commands in YAML or use
        // getCommand(); register /vesta through the Brigadier lifecycle API instead.
        // The legacy plugin.yml/getCommand path lives in BukkitEntrypoint.
        try {
            this.getLifecycleManager().registerEventHandler(
                    io.papermc.paper.plugin.lifecycle.event.types.LifecycleEvents.COMMANDS,
                    event -> {
                        event.registrar().register(
                                io.papermc.paper.command.brigadier.Commands.literal("vesta")
                                        .executes(ctx -> {
                                            sendVestaHelp(ctx.getSource().getSender());
                                            return com.mojang.brigadier.Command.SINGLE_SUCCESS;
                                        })
                                        .then(io.papermc.paper.command.brigadier.Commands.literal("help")
                                                .executes(ctx -> {
                                                    sendVestaHelp(ctx.getSource().getSender());
                                                    return com.mojang.brigadier.Command.SINGLE_SUCCESS;
                                                }))
                                        .then(io.papermc.paper.command.brigadier.Commands.literal("status")
                                                .executes(ctx -> {
                                                    var sender = ctx.getSource().getSender();
                                                    boolean wrapper = com.vesta.loader.VestaSyncManager.get()
                                                            .isWrapperDetected();
                                                    sender.sendMessage("Vesta sync active. Wrapper IPC: "
                                                            + (wrapper ? "connected" : "standalone (embedded HTTP)")
                                                            + ".");
                                                    return com.mojang.brigadier.Command.SINGLE_SUCCESS;
                                                }))
                                        .then(io.papermc.paper.command.brigadier.Commands.literal("link")
                                                .executes(ctx -> {
                                                    var sender = ctx.getSource().getSender();
                                                    if (!(sender instanceof org.bukkit.entity.Player player)) {
                                                        sender.sendMessage("Players only.");
                                                        return com.mojang.brigadier.Command.SINGLE_SUCCESS;
                                                    }
                                                    String code = com.vesta.loader.VestaSyncManager.get()
                                                            .createLinkCode(player.getUniqueId());
                                                    sender.sendMessage("Vesta link code: " + code
                                                            + " (expires in ~5 minutes). Enter it in the launcher.");
                                                    return com.mojang.brigadier.Command.SINGLE_SUCCESS;
                                                }))
                                        .build(),
                                "Vesta sync commands (help, status, link)"
                        );
                        getLogger().info("Registered /vesta command (help, status, link) via Paper lifecycle API.");
                    }
            );
        } catch (Throwable t) {
            getLogger().warning("Could not register /vesta command: " + t.getMessage()
                    + ". Account linking via command is disabled; sync + control IPC still active.");
        }
    }

    private static void sendVestaHelp(org.bukkit.command.CommandSender sender) {
        sender.sendMessage("Vesta sync commands:");
        sender.sendMessage("  /vesta status  - show sync/wrapper connection status");
        sender.sendMessage("  /vesta link    - get a code to link your account in the launcher");
    }

    @Override
    public void onDisable() {
        if (mod != null) {
            mod.onServerStop();
        }
    }
}
