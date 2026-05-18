package com.vesta.loader.platform;

import com.vesta.loader.VestaMod;

@net.neoforged.fml.common.Mod(VestaMod.MOD_ID)
public class NeoForgeEntrypoint {
    public NeoForgeEntrypoint() {
        new VestaMod().onInitialize();
    }
}
