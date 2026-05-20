package com.vesta.sync.protocol;

public final class Disconnect {
    public String type = "disconnect";
    public long seq;
    public String session;
    public String ts;
    public String reason;
}
