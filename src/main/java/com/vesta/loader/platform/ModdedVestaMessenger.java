package com.vesta.loader.platform;

import com.mojang.brigadier.CommandDispatcher;
import com.mojang.brigadier.builder.LiteralArgumentBuilder;
import com.vesta.loader.VestaCommands;
import net.minecraft.commands.CommandSourceStack;
import net.minecraft.commands.Commands;
import net.minecraft.server.level.ServerPlayer;

import java.util.UUID;

final class ModdedVestaMessenger implements VestaCommands.Messenger {

    private final CommandSourceStack source;

    ModdedVestaMessenger(CommandSourceStack source) {
        this.source = source;
    }

    @Override
    public void sendMessage(String message) {
        source.sendSuccess(() -> net.minecraft.network.chat.Component.literal(message), false);
    }

    @Override
    public boolean isPlayer() {
        return source.getEntity() instanceof ServerPlayer;
    }

    @Override
    public UUID getPlayerUuid() {
        return ((ServerPlayer) source.getEntity()).getUUID();
    }

    static void register(CommandDispatcher<CommandSourceStack> dispatcher) {
        LiteralArgumentBuilder<CommandSourceStack> root = Commands.literal("vesta")
                .executes(ctx -> {
                    VestaCommands.sendHelp(new ModdedVestaMessenger(ctx.getSource()));
                    return 1;
                })
                .then(Commands.literal("help")
                        .executes(ctx -> {
                            VestaCommands.sendHelp(new ModdedVestaMessenger(ctx.getSource()));
                            return 1;
                        }))
                .then(Commands.literal("status")
                        .executes(ctx -> {
                            VestaCommands.sendStatus(new ModdedVestaMessenger(ctx.getSource()));
                            return 1;
                        }))
                .then(Commands.literal("link")
                        .executes(ctx -> {
                            VestaCommands.sendLink(new ModdedVestaMessenger(ctx.getSource()));
                            return 1;
                        }));
        dispatcher.register(root);
    }
}
