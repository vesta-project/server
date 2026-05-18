package com.vesta.loader.platform;

import org.quiltmc.loader.api.ModInitializer;
import com.vesta.loader.VestaMod;

public class QuiltEntrypoint implements ModInitializer {
    @Override
    public void onInitialize(org.quiltmc.loader.api.ModContainer mod) {
        new VestaMod().onInitialize();
    }
}
