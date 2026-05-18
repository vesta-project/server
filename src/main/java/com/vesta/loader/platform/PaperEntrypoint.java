package com.vesta.loader.platform;

import io.papermc.paper.plugin.lifecycle.event.LifecycleEventManager;
import io.papermc.paper.plugin.lifecycle.event.types.LifecycleEvents;
import org.bukkit.plugin.java.JavaPlugin;
import com.vesta.loader.VestaMod;

public class PaperEntrypoint extends JavaPlugin {
    @Override
    public void onEnable() {
        new VestaMod().onInitialize();
    }
}
