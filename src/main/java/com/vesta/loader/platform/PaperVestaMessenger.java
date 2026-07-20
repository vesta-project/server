package com.vesta.loader.platform;

import com.mojang.brigadier.builder.LiteralArgumentBuilder;
import com.vesta.loader.VestaCommands;
import io.papermc.paper.command.brigadier.CommandSourceStack;
import io.papermc.paper.command.brigadier.Commands;
import org.bukkit.command.CommandSender;
import org.bukkit.entity.Player;

import java.util.UUID;

final class PaperVestaMessenger implements VestaCommands.Messenger {

    private final CommandSender sender;

    PaperVestaMessenger(CommandSender sender) {
        this.sender = sender;
    }

    @Override
    public void sendMessage(String message) {
        sender.sendMessage(message);
    }

    @Override
    public boolean isPlayer() {
        return sender instanceof Player;
    }

    @Override
    public UUID getPlayerUuid() {
        return ((Player) sender).getUniqueId();
    }

    static LiteralArgumentBuilder<CommandSourceStack> register(Commands registrar) {
        return registrar.literal("vesta")
                .executes(ctx -> {
                    VestaCommands.sendHelp(new PaperVestaMessenger(ctx.getSource().getSender()));
                    return com.mojang.brigadier.Command.SINGLE_SUCCESS;
                })
                .then(registrar.literal("help")
                        .executes(ctx -> {
                            VestaCommands.sendHelp(new PaperVestaMessenger(ctx.getSource().getSender()));
                            return com.mojang.brigadier.Command.SINGLE_SUCCESS;
                        }))
                .then(registrar.literal("status")
                        .executes(ctx -> {
                            VestaCommands.sendStatus(new PaperVestaMessenger(ctx.getSource().getSender()));
                            return com.mojang.brigadier.Command.SINGLE_SUCCESS;
                        }))
                .then(registrar.literal("link")
                        .executes(ctx -> {
                            VestaCommands.sendLink(new PaperVestaMessenger(ctx.getSource().getSender()));
                            return com.mojang.brigadier.Command.SINGLE_SUCCESS;
                        }));
    }
}
