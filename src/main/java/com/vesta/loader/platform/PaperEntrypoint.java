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
            org.bukkit.entity.Player online = Bukkit.getPlayer(uuid);
            if (online != null) {
                return online.hasPermission(permission);
            }
            return false;
        });

        try {
            this.getLifecycleManager().registerEventHandler(
                    io.papermc.paper.plugin.lifecycle.event.types.LifecycleEvents.COMMANDS,
                    event -> {
                        event.registrar().register(
                                PaperVestaMessenger.register(event.registrar()),
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

    @Override
    public void onDisable() {
        if (mod != null) {
            mod.onServerStop();
        }
    }
}
