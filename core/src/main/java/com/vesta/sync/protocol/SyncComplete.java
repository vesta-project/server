package com.vesta.sync.protocol;

public final class SyncComplete {
    public String type = "sync_complete";
    public long seq;
    public String session;
    public String ts;
    public String status;
    public String errorMessage;
}
