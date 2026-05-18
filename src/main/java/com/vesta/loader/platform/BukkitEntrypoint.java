package com.vesta.loader.platform;

import org.bukkit.plugin.java.JavaPlugin;
import com.vesta.loader.VestaMod;

public class BukkitEntrypoint extends JavaPlugin {
    @Override
    public void onEnable() {
        new VestaMod().onInitialize();
    }
}
