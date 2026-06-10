package com.vesta.loader.platform;

import com.vesta.loader.VestaMod;

@net.neoforged.fml.common.Mod(VestaMod.MOD_ID)
public class NeoForgeEntrypoint {
    public NeoForgeEntrypoint() {
        VestaMod mod = new VestaMod();
        mod.onInitialize();

        boolean isServer = false;
        try {
            isServer = net.neoforged.fml.loading.FMLEnvironment.dist.isServer();
        } catch (Throwable ignored) {
            isServer = true;
        }

        if (isServer) {
            java.nio.file.Path gameDir = java.nio.file.Path.of(System.getProperty("user.dir"));
            String mcVersion = "unknown";
            try {
                mcVersion = net.neoforged.fml.loading.FMLLoader.versionInfo().mcVersion();
            } catch (Throwable ignored) {}

            mod.onServerStart(
                gameDir,
                "Server Modpack",
                "1.0.0",
                mcVersion,
                "neoforge",
                "latest",
                "0.0.0.0",
                25565
            );
        }
    }
}
