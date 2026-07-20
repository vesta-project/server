package com.vesta.loader.platform;

import com.vesta.loader.VestaMod;
import net.neoforged.bus.api.SubscribeEvent;
import net.neoforged.fml.common.EventBusSubscriber;
import net.neoforged.neoforge.event.RegisterCommandsEvent;

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

    @EventBusSubscriber(modid = VestaMod.MOD_ID, bus = EventBusSubscriber.Bus.GAME)
    public static final class Commands {
        private Commands() {}

        @SubscribeEvent
        public static void register(RegisterCommandsEvent event) {
            ModdedVestaMessenger.register(event.getDispatcher());
        }
    }
}
