package com.vesta.sync.protocol;

import com.google.gson.Gson;
import com.google.gson.JsonObject;
import com.google.gson.JsonParser;

import java.io.*;
import java.nio.charset.StandardCharsets;
import java.time.Instant;
import java.util.HashMap;
import java.util.Map;
import java.util.UUID;

public final class TcpMessageCodec {

    private static final Gson GSON = new com.google.gson.GsonBuilder()
            .setFieldNamingPolicy(com.google.gson.FieldNamingPolicy.LOWER_CASE_WITH_UNDERSCORES)
            .create();
    private static final Map<String, Class<?>> REGISTRY = new HashMap<>();

    static {
        register("handshake", Handshake.class);
        register("handshake_response", HandshakeResponse.class);
        register("sync_check", SyncCheck.class);
        register("sync_check_response", SyncCheckResponse.class);
        register("file_request", FileRequest.class);
        register("file_response", FileResponse.class);
        register("push_update_available", PushUpdateAvailable.class);
        register("heartbeat", Heartbeat.class);
        register("sync_complete", SyncComplete.class);
        register("disconnect", Disconnect.class);
    }

    private static void register(String type, Class<?> clazz) {
        REGISTRY.put(type, clazz);
    }

    public static Class<?> resolve(String type) {
        return REGISTRY.get(type);
    }

    public static <T> byte[] encode(T message) {
        String json = GSON.toJson(message);
        byte[] jsonBytes = json.getBytes(StandardCharsets.UTF_8);
        if (jsonBytes.length > com.vesta.sync.VestaSync.MAX_MESSAGE_BYTES) {
            throw new IllegalArgumentException("Message exceeds max size: " + jsonBytes.length);
        }
        byte[] frame = new byte[4 + jsonBytes.length];
        frame[0] = (byte) (jsonBytes.length >>> 24);
        frame[1] = (byte) (jsonBytes.length >>> 16);
        frame[2] = (byte) (jsonBytes.length >>> 8);
        frame[3] = (byte) (jsonBytes.length);
        System.arraycopy(jsonBytes, 0, frame, 4, jsonBytes.length);
        return frame;
    }

    public static Object decode(InputStream in) throws IOException {
        byte[] lenBuf = readExactly(in, 4);
        if (lenBuf == null) {
            return null;
        }
        int length = ((lenBuf[0] & 0xFF) << 24)
                   | ((lenBuf[1] & 0xFF) << 16)
                   | ((lenBuf[2] & 0xFF) << 8)
                   |  (lenBuf[3] & 0xFF);
        if (length < 0 || length > com.vesta.sync.VestaSync.MAX_MESSAGE_BYTES) {
            throw new IOException("Invalid message length: " + length);
        }
        byte[] jsonBytes = readExactly(in, length);
        if (jsonBytes == null) {
            throw new IOException("Unexpected end of stream reading message body");
        }
        String json = new String(jsonBytes, StandardCharsets.UTF_8);
        JsonObject obj = JsonParser.parseString(json).getAsJsonObject();
        String type = obj.get("type").getAsString();
        Class<?> clazz = resolve(type);
        if (clazz == null) {
            throw new IOException("Unknown message type: " + type);
        }
        return GSON.fromJson(json, clazz);
    }

    private static byte[] readExactly(InputStream in, int count) throws IOException {
        byte[] buf = new byte[count];
        int offset = 0;
        while (offset < count) {
            int read = in.read(buf, offset, count - offset);
            if (read < 0) {
                return null;
            }
            offset += read;
        }
        return buf;
    }

    // convenience builders

    public static String now() {
        return Instant.now().toString();
    }

    public static String newSession() {
        return UUID.randomUUID().toString();
    }

    private TcpMessageCodec() {}
}
