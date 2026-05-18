package com.vesta.loader.platform;

import net.fabricmc.api.ModInitializer;
import com.vesta.loader.VestaMod;

public class FabricEntrypoint implements ModInitializer {
    @Override
    public void onInitialize() {
        new VestaMod().onInitialize();
    }
}
