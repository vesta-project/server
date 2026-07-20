package com.vesta.loader.platform;

import net.fabricmc.fabric.api.command.v2.CommandRegistrationCallback;

final class FabricCommandHooks {
    private FabricCommandHooks() {}

    static void register() {
        CommandRegistrationCallback.EVENT.register(
                (dispatcher, registryAccess, environment) -> ModdedVestaMessenger.register(dispatcher));
    }
}
