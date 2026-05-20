package com.vesta.sync;

public final class VestaSync {
    public static final String API_VERSION = "1.0.0";

    public static final String API_MANIFEST = "/api/v1/modpack/manifest";
    public static final String API_MODPACK_INFO = "/api/v1/modpack/info";
    public static final String API_MODPACK_VERSIONS = "/api/v1/modpack/versions";
    public static final String API_SYNC_CHECK = "/api/v1/sync/check";
    public static final String API_FILES = "/api/v1/files/";

    public static final int DEFAULT_HTTP_PORT = 9090;
    public static final int DEFAULT_TCP_PORT = 9091;

    public static final int PROTOCOL_VERSION = 1;
    public static final int FRAME_LENGTH_BYTES = 4;
    public static final int MAX_MESSAGE_BYTES = 16 * 1024 * 1024;

    public static final int HEARTBEAT_INTERVAL_SEC = 30;
    public static final int HEARTBEAT_TIMEOUT_SEC = 90;

    public static final String SESSION_HEADER = "X-Vesta-Session";

    private VestaSync() {}
}
