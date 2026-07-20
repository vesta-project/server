package com.vesta.loader.platform;

import org.quiltmc.loader.api.DedicatedServerModInitializer;
import org.quiltmc.loader.api.ModInitializer;
import org.quiltmc.loader.api.QuiltLoader;
import com.vesta.loader.VestaMod;

public class QuiltEntrypoint implements ModInitializer, DedicatedServerModInitializer {
    @Override
    public void onInitialize(org.quiltmc.loader.api.ModContainer mod) {
        new VestaMod().onInitialize();
    }

    @Override
    public void onInitializeServer(org.quiltmc.loader.api.ModContainer mod) {
        String mcVersion = QuiltLoader.getGameInstance()
            .map(g -> g.getGameVersion())
            .orElse("unknown");
        new VestaMod().onServerStart(
            QuiltLoader.getGameDir(),
            "Server Modpack",
            "1.0.0",
            mcVersion,
            "quilt",
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
            // fabric-api command module not on the classpath
        }
    }
}
