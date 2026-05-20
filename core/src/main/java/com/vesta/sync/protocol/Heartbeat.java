package com.vesta.sync.protocol;

public final class Heartbeat {
    public String type = "heartbeat";
    public long seq;
    public String session;
    public String ts;
}
