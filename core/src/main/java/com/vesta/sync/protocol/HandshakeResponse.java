package com.vesta.sync.protocol;

public final class HandshakeResponse {
    public String type = "handshake_response";
    public long seq;
    public String session;
    public String ts;
    public String status;
    public String errorMessage;
    public String serverVersion;
    public long sessionTtlSeconds;
}
