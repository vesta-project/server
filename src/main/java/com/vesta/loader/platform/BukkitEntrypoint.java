package com.vesta.loader.platform;

import org.bukkit.plugin.java.JavaPlugin;
import org.bukkit.Bukkit;
import com.vesta.loader.VestaMod;
import com.vesta.loader.VestaCommands;
import java.nio.file.Path;

public class BukkitEntrypoint extends JavaPlugin {
    @Override
    public void onEnable() {
        VestaMod mod = new VestaMod();
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
            "bukkit",
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
            return player.hasPermission(permission);
        });

        var cmd = getCommand("vesta");
        if (cmd != null) {
            cmd.setExecutor((sender, command, label, args) -> {
                BukkitVestaMessenger messenger = new BukkitVestaMessenger(sender);
                if (args.length == 0 || "help".equalsIgnoreCase(args[0])) {
                    VestaCommands.sendHelp(messenger);
                    return true;
                }
                if ("status".equalsIgnoreCase(args[0])) {
                    VestaCommands.sendStatus(messenger);
                    return true;
                }
                if ("link".equalsIgnoreCase(args[0])) {
                    VestaCommands.sendLink(messenger);
                    return true;
                }
                VestaCommands.sendHelp(messenger);
                return true;
            });
            getLogger().info("Registered /vesta command (help, status, link).");
        }
    }

    @Override
    public void onDisable() {
        new VestaMod().onServerStop();
    }
}
