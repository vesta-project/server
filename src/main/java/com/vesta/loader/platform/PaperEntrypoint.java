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
            return player.hasPermission(permission);
        });

        var cmd = getCommand("vesta");
        if (cmd != null) {
            cmd.setExecutor((sender, command, label, args) -> {
                if (args.length >= 1 && "link".equalsIgnoreCase(args[0])) {
                    if (!(sender instanceof org.bukkit.entity.Player player)) {
                        sender.sendMessage("Players only.");
                        return true;
                    }
                    String code = com.vesta.loader.VestaSyncManager.get().createLinkCode(player.getUniqueId());
                    sender.sendMessage("Vesta link code: " + code + " (expires in ~5 minutes). Enter it in the launcher.");
                    return true;
                }
                sender.sendMessage("Usage: /vesta link");
                return true;
            });
        }
    }

    @Override
    public void onDisable() {
        if (mod != null) {
            mod.onServerStop();
        }
    }
}
