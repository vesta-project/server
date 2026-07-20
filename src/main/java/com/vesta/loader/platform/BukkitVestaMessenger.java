package com.vesta.loader.platform;

import com.vesta.loader.VestaCommands;
import org.bukkit.command.CommandSender;
import org.bukkit.entity.Player;

import java.util.UUID;

final class BukkitVestaMessenger implements VestaCommands.Messenger {

    private final CommandSender sender;

    BukkitVestaMessenger(CommandSender sender) {
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
}
