package com.vesta.loader.platform;

import org.bukkit.plugin.java.JavaPlugin;
import org.bukkit.Bukkit;
import com.vesta.loader.VestaMod;
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
        new VestaMod().onServerStop();
    }
}
