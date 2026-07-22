package com.vesta.sync;

public final class VestaSync {
    public static final String API_VERSION = "1.0.0";

    public static final String API_MANIFEST = "/api/v1/modpack/manifest";
    public static final String API_MODPACK_INFO = "/api/v1/modpack/info";
    public static final String API_MODPACK_VERSIONS = "/api/v1/modpack/versions";
    public static final String API_MODPACK_ARCHIVE = "/api/v1/modpack/archive";
    public static final String API_SYNC_CHECK = "/api/v1/sync/check";
    public static final String API_FILES = "/api/v1/files/";

    public static final String API_AUTH_TOKEN = "/api/v1/auth/token";
    public static final String API_AUTH_LINK = "/api/v1/auth/link";
    public static final String API_PRIVATE_FILES = "/api/v1/private/files";
    public static final String API_FILES_UPLOAD = "/api/v1/files/upload";
    public static final String API_CONTENT_REMOVE = "/api/v1/content/remove";
    public static final String API_STATUS = "/api/v1/status";

    public static final String API_SERVER_DESCRIPTOR = "/api/v1/server/descriptor";
    public static final String API_SERVER_POWER = "/api/v1/server/power";
    public static final String API_ADMIN_SECURITY = "/api/v1/admin/security";
    public static final String API_AUTH_MINECRAFT_CHALLENGE = "/api/v1/auth/minecraft/challenge";
    public static final String API_AUTH_MINECRAFT_COMPLETE = "/api/v1/auth/minecraft/complete";
    public static final String API_REALTIME_TICKET = "/api/v1/realtime/ticket";
    public static final String API_REALTIME = "/api/v1/realtime";
    public static final String API_CONSOLE = "/api/v1/console";
    public static final String API_FS = "/api/v1/fs";
    public static final String API_FS_PREFIX = "/api/v1/fs/";

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
