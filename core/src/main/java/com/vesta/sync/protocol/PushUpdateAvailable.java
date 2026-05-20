package com.vesta.sync.protocol;

public final class PushUpdateAvailable {
    public String type = "push_update_available";
    public long seq;
    public String session;
    public String ts;
    public String newVersion;
    public String changelog;
    public boolean requiresRestart;
    public boolean forceUpdate;
}
