package com.vesta.sync.ipc;

import com.google.gson.JsonObject;
import org.junit.jupiter.api.Test;

import java.io.ByteArrayInputStream;
import java.io.ByteArrayOutputStream;
import java.io.DataInputStream;
import java.io.DataOutputStream;

import static org.junit.jupiter.api.Assertions.*;

class ControlCodecTest {

    @Test
    void roundTrip() throws Exception {
        ByteArrayOutputStream bos = new ByteArrayOutputStream();
        DataOutputStream out = new DataOutputStream(bos);
        ControlCodec.write(out, ControlCodec.permissionCheck(java.util.UUID.randomUUID().toString(), "vesta.mods.install"));

        DataInputStream in = new DataInputStream(new ByteArrayInputStream(bos.toByteArray()));
        JsonObject msg = ControlCodec.read(in);
        assertEquals("permission_check", msg.get("type").getAsString());
        assertEquals("vesta.mods.install", msg.get("permission").getAsString());
    }
}
