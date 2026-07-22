package com.vesta.sync.protocol;

public final class PushUpdateAvailable {
    public String type = "push_update_available";
    public long seq;
    public String session;
    public String ts;
    /** Current manifest revision (SHA-1/pack revision analogue); sent to clients for cache/polling. */
    public long revision;
    public String newVersion;
    /** Previous version string the client was on when the push was generated. */
    public String previousVersion;
    public String changelog;
    public boolean requiresRestart;
    public boolean forceUpdate;
}
