package com.vesta.sync.protocol;

import java.util.Collections;
import java.util.List;

public final class Handshake {
    public String type = "handshake";
    public long seq;
    public String session = "";
    public String ts;
    public String version;
    public String clientId;
    public String authToken = "";
    public String password = "";
    public List<String> capabilities = Collections.emptyList();
}
