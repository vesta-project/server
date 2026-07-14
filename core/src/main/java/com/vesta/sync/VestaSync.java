package com.vesta.sync;

public final class VestaSync {
    public static final String API_VERSION = "1.0.0";

    public static final String API_MANIFEST = "/api/v1/modpack/manifest";
    public static final String API_MODPACK_INFO = "/api/v1/modpack/info";
    public static final String API_MODPACK_VERSIONS = "/api/v1/modpack/versions";
    public static final String API_SYNC_CHECK = "/api/v1/sync/check";
    public static final String API_FILES = "/api/v1/files/";

    public static final String API_AUTH_TOKEN = "/api/v1/auth/token";
    public static final String API_AUTH_LINK = "/api/v1/auth/link";
    public static final String API_FILES_UPLOAD = "/api/v1/files/upload";
    public static final String API_CONTENT_REMOVE = "/api/v1/content/remove";
    public static final String API_STATUS = "/api/v1/status";

    public static final int DEFAULT_HTTP_PORT = 9090;
    public static final int DEFAULT_TCP_PORT = 9090;
    public static final int DEFAULT_CONTROL_PORT = 9091;

    public static final int PROTOCOL_VERSION = 1;
    public static final int FRAME_LENGTH_BYTES = 4;
    public static final int MAX_MESSAGE_BYTES = 16 * 1024 * 1024;

    public static final int HEARTBEAT_INTERVAL_SEC = 30;
    public static final int HEARTBEAT_TIMEOUT_SEC = 90;

    public static final String SESSION_HEADER = "X-Vesta-Session";
    public static final String AUTH_HEADER = "Authorization";
    public static final String BEARER_PREFIX = "Bearer ";

    private VestaSync() {}
}
