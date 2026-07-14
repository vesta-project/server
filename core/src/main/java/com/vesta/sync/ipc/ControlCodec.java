package com.vesta.sync.ipc;

import com.google.gson.Gson;
import com.google.gson.JsonObject;

import java.io.DataInputStream;
import java.io.DataOutputStream;
import java.io.IOException;
import java.nio.charset.StandardCharsets;

/**
 * Length-prefixed JSON framing for wrapper ↔ mod control channel.
 */
public final class ControlCodec {

    private static final Gson GSON = new Gson();

    private ControlCodec() {}

    public static void write(DataOutputStream out, JsonObject message) throws IOException {
        byte[] bytes = GSON.toJson(message).getBytes(StandardCharsets.UTF_8);
        out.writeInt(bytes.length);
        out.write(bytes);
        out.flush();
    }

    public static JsonObject read(DataInputStream in) throws IOException {
        int len = in.readInt();
        if (len <= 0 || len > 1024 * 1024) {
            throw new IOException("Invalid control frame length: " + len);
        }
        byte[] bytes = new byte[len];
        in.readFully(bytes);
        return GSON.fromJson(new String(bytes, StandardCharsets.UTF_8), JsonObject.class);
    }

    public static JsonObject heartbeat() {
        JsonObject o = new JsonObject();
        o.addProperty("type", "heartbeat");
        o.addProperty("ts", System.currentTimeMillis());
        return o;
    }

    public static JsonObject heartbeatAck() {
        JsonObject o = new JsonObject();
        o.addProperty("type", "heartbeat_ack");
        o.addProperty("ts", System.currentTimeMillis());
        return o;
    }

    public static JsonObject permissionCheck(String uuid, String permission) {
        JsonObject o = new JsonObject();
        o.addProperty("type", "permission_check");
        o.addProperty("uuid", uuid);
        o.addProperty("permission", permission);
        return o;
    }

    public static JsonObject permissionResult(boolean allowed, String reason) {
        JsonObject o = new JsonObject();
        o.addProperty("type", "permission_result");
        o.addProperty("allowed", allowed);
        if (reason != null) {
            o.addProperty("reason", reason);
        }
        return o;
    }

    public static JsonObject linkCode(String code, String uuid) {
        JsonObject o = new JsonObject();
        o.addProperty("type", "link_code");
        o.addProperty("code", code);
        o.addProperty("uuid", uuid);
        return o;
    }

    public static JsonObject error(String message) {
        JsonObject o = new JsonObject();
        o.addProperty("type", "error");
        o.addProperty("message", message);
        return o;
    }
}
