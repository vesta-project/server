package com.vesta.sync.protocol;

import org.junit.jupiter.api.Test;

import java.io.ByteArrayInputStream;
import java.io.ByteArrayOutputStream;
import java.io.IOException;
import java.util.Collections;
import java.util.HashMap;

import static org.junit.jupiter.api.Assertions.*;

class TcpMessageCodecTest {

    @Test
    void encodeHandshake() {
        Handshake msg = new Handshake();
        msg.seq = 0;
        msg.ts = "2026-01-01T00:00:00Z";
        msg.version = "1.0.0";
        msg.clientId = "test-client";
        msg.capabilities = java.util.Arrays.asList("sync_v1", "delta_v1");

        byte[] frame = TcpMessageCodec.encode(msg);
        String json = new String(frame, 4, frame.length - 4);
        assertTrue(json.contains("\"type\":\"handshake\""));
        assertTrue(json.contains("\"client_id\":\"test-client\""));
        assertTrue(json.contains("\"capabilities\""));
    }

    @Test
    void roundtripHandshake() throws IOException {
        Handshake out = new Handshake();
        out.seq = 1;
        out.ts = "2026-01-01T00:00:00Z";
        out.version = "1.0.0";
        out.clientId = "test-client";
        out.capabilities = Collections.singletonList("sync_v1");

        byte[] frame = TcpMessageCodec.encode(out);
        ByteArrayInputStream in = new ByteArrayInputStream(frame);
        Object decoded = TcpMessageCodec.decode(in);
        assertTrue(decoded instanceof Handshake);
        Handshake in2 = (Handshake) decoded;
        assertEquals(1L, in2.seq);
        assertEquals("test-client", in2.clientId);
        assertEquals(Collections.singletonList("sync_v1"), in2.capabilities);
        assertTrue(in2.ts.contains("2026-"));
    }

    @Test
    void roundtripAllMessageTypes() throws IOException {
        // HandshakeResponse
        HandshakeResponse hr = new HandshakeResponse();
        hr.status = "ok";
        hr.session = TcpMessageCodec.newSession();
        hr.serverVersion = "1.0.0";
        hr.sessionTtlSeconds = 3600;
        roundtrip(hr);

        // SyncCheck
        SyncCheck sc = new SyncCheck();
        sc.seq = 1;
        sc.session = hr.session;
        sc.ts = TcpMessageCodec.now();
        sc.mode = "custom";
        sc.source = "server";
        sc.currentVersion = "1.0.0";
        sc.installedHashes = Collections.singletonMap("mods/test.jar", "abcdef1234567890");
        roundtrip(sc);

        // Heartbeat
        Heartbeat hb = new Heartbeat();
        hb.seq = 10;
        hb.session = hr.session;
        hb.ts = TcpMessageCodec.now();
        roundtrip(hb);

        // Disconnect
        Disconnect dc = new Disconnect();
        dc.seq = 99;
        dc.session = hr.session;
        dc.ts = TcpMessageCodec.now();
        dc.reason = "shutdown";
        roundtrip(dc);
    }

    @Test
    void registerResolvesKnownTypes() {
        assertEquals(Handshake.class, TcpMessageCodec.resolve("handshake"));
        assertEquals(HandshakeResponse.class, TcpMessageCodec.resolve("handshake_response"));
        assertEquals(SyncCheck.class, TcpMessageCodec.resolve("sync_check"));
        assertEquals(SyncCheckResponse.class, TcpMessageCodec.resolve("sync_check_response"));
        assertEquals(FileRequest.class, TcpMessageCodec.resolve("file_request"));
        assertEquals(FileResponse.class, TcpMessageCodec.resolve("file_response"));
        assertEquals(PushUpdateAvailable.class, TcpMessageCodec.resolve("push_update_available"));
        assertEquals(Heartbeat.class, TcpMessageCodec.resolve("heartbeat"));
        assertEquals(SyncComplete.class, TcpMessageCodec.resolve("sync_complete"));
        assertEquals(Disconnect.class, TcpMessageCodec.resolve("disconnect"));
        assertNull(TcpMessageCodec.resolve("nonexistent"));
    }

    @Test
    void encodeLengthPrefixCorrect() {
        Heartbeat hb = new Heartbeat();
        hb.seq = 1;
        hb.session = "s";
        hb.ts = "t";
        byte[] frame = TcpMessageCodec.encode(hb);
        int declared = ((frame[0] & 0xFF) << 24) | ((frame[1] & 0xFF) << 16) | ((frame[2] & 0xFF) << 8) | (frame[3] & 0xFF);
        assertEquals(frame.length - 4, declared);
    }

    @Test
    void encodeThrowsOnOversize() {
        Handshake msg = new Handshake();
        msg.capabilities = Collections.nCopies(5_000_000, "x");
        assertThrows(IllegalArgumentException.class, () -> TcpMessageCodec.encode(msg));
    }

    @Test
    void decodeReturnsNullOnEmptyStream() throws IOException {
        assertNull(TcpMessageCodec.decode(new ByteArrayInputStream(new byte[0])));
    }

    @Test
    void decodeThrowsOnUnknownType() {
        String json = "{\"type\":\"bogus\"}";
        byte[] jsonBytes = json.getBytes(java.nio.charset.StandardCharsets.UTF_8);
        byte[] len = new byte[4];
        len[0] = (byte) (jsonBytes.length >>> 24);
        len[1] = (byte) (jsonBytes.length >>> 16);
        len[2] = (byte) (jsonBytes.length >>> 8);
        len[3] = (byte) jsonBytes.length;
        ByteArrayOutputStream bos = new ByteArrayOutputStream();
        try {
            bos.write(len);
            bos.write(jsonBytes);
        } catch (IOException e) {
            fail("Unexpected IOException");
        }
        ByteArrayInputStream in = new ByteArrayInputStream(bos.toByteArray());
        assertThrows(IOException.class, () -> TcpMessageCodec.decode(in));
    }

    private void roundtrip(Object msg) throws IOException {
        byte[] frame = TcpMessageCodec.encode(msg);
        Object decoded = TcpMessageCodec.decode(new ByteArrayInputStream(frame));
        assertNotNull(decoded);
        assertEquals(msg.getClass(), decoded.getClass());
    }
}
