package com.vesta.loader;

import java.util.UUID;

/**
 * Shared /vesta command text and link-code creation for all loader entrypoints.
 */
public final class VestaCommands {

    private VestaCommands() {}

    public interface Messenger {
        void sendMessage(String message);

        boolean isPlayer();

        UUID getPlayerUuid();
    }

    public static void sendHelp(Messenger messenger) {
        messenger.sendMessage("Vesta sync commands:");
        messenger.sendMessage("  /vesta status  - show sync/wrapper connection status");
        messenger.sendMessage("  /vesta link    - get a code to link your account in the launcher");
    }

    public static void sendStatus(Messenger messenger) {
        boolean wrapper = VestaSyncManager.get().isWrapperMode();
        messenger.sendMessage("Vesta sync active. Mode: "
                + (wrapper ? "wrapper (control IPC)" : "standalone (embedded HTTP)"));
    }

    public static void sendLink(Messenger messenger) {
        if (!messenger.isPlayer()) {
            messenger.sendMessage("Players only.");
            return;
        }
        String code = VestaSyncManager.get().createLinkCode(messenger.getPlayerUuid());
        messenger.sendMessage("Vesta link code: " + code
                + " (expires in ~5 minutes). Enter it in the launcher.");
    }
}
