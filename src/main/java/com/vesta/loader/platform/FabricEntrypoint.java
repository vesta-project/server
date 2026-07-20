package com.vesta.loader.platform;

import net.fabricmc.api.DedicatedServerModInitializer;
import net.fabricmc.api.ModInitializer;
import net.fabricmc.loader.api.FabricLoader;
import com.vesta.loader.VestaMod;

public class FabricEntrypoint implements ModInitializer, DedicatedServerModInitializer {
    @Override
    public void onInitialize() {
        new VestaMod().onInitialize();
    }

    @Override
    public void onInitializeServer() {
        String mcVersion = FabricLoader.getInstance()
            .getModContainer("minecraft")
            .map(c -> c.getMetadata().getVersion().getFriendlyString())
            .orElse("unknown");
        new VestaMod().onServerStart(
            FabricLoader.getInstance().getGameDir(),
            "Server Modpack",
            "1.0.0",
            mcVersion,
            "fabric",
            "latest",
            "0.0.0.0",
            25565
        );
        registerCommandsReflectively();
    }

    private static void registerCommandsReflectively() {
        try {
            Class.forName("com.vesta.loader.platform.FabricCommandHooks")
                    .getMethod("register")
                    .invoke(null);
        } catch (ReflectiveOperationException | LinkageError ignored) {
            // fabric-api command module not on the classpath (e.g. fabric-26 snapshot builds)
        }
    }
}
