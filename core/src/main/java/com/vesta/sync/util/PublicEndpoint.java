package com.vesta.sync.util;

import com.vesta.sync.VestaSync;

public final class PublicEndpoint {

    private final String scheme;
    private final String host;
    private final int port;

    public PublicEndpoint(String scheme, String host, int port) {
        this.scheme = scheme == null || scheme.trim().isEmpty() ? "http" : scheme;
        this.host = host == null || host.trim().isEmpty() ? "localhost" : host;
        this.port = port;
    }

    public String fileUrl(String sha256) {
        return scheme + "://" + host + ":" + port + VestaSync.API_FILES + sha256;
    }

    public String baseUrl() {
        return scheme + "://" + host + ":" + port;
    }
}
