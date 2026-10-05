package com.vesta.wrapper.protocol;

import com.google.gson.JsonObject;
import com.google.gson.JsonParser;
import com.vesta.wrapper.protocol.ServerFeatureProtocol.ProtocolException;
import org.junit.jupiter.api.DynamicTest;
import org.junit.jupiter.api.TestFactory;

import java.io.IOException;
import java.nio.charset.StandardCharsets;
import java.util.Arrays;
import java.util.HexFormat;
import java.util.stream.Stream;
import java.util.stream.StreamSupport;

import static org.junit.jupiter.api.Assertions.*;

/** Same physical adversarial corpus used by the independently implemented Node peer. */
class ServerFeatureConformanceTest {
    static byte[] fixture(String name) throws IOException {
        try (var input = ServerFeatureConformanceTest.class.getResourceAsStream("/v1/fixtures/" + name)) {
            assertNotNull(input, name);
            return input.readAllBytes();
        }
    }

    @TestFactory Stream<DynamicTest> sharedCorpus() throws IOException {
        var corpus = JsonParser.parseString(new String(fixture("corpus.json"), StandardCharsets.UTF_8)).getAsJsonArray();
        return StreamSupport.stream(corpus.spliterator(), false).map(value -> {
            JsonObject row = value.getAsJsonObject();
            return DynamicTest.dynamicTest(row.get("name").getAsString(), () -> {
                byte[] bytes = row.has("hex") ? HexFormat.of().parseHex(row.get("hex").getAsString())
                        : row.get("json").getAsString().getBytes(StandardCharsets.UTF_8);
                if (row.has("padTo")) {
                    byte[] padded = new byte[row.get("padTo").getAsInt()];
                    Arrays.fill(padded, (byte) ' ');
                    System.arraycopy(bytes, 0, padded, padded.length - bytes.length, bytes.length);
                    bytes = padded;
                }
                String kind = row.get("kind").getAsString();
                if (row.has("error")) {
                    byte[] input = bytes;
                    ProtocolException failure = assertThrows(ProtocolException.class, () -> roundtrip(kind, input));
                    String code = row.get("error").getAsString();
                    assertEquals(code, failure.code().wire());
                    assertEquals("Server feature protocol failed: " + code, failure.getMessage());
                    assertNull(failure.getCause());
                } else {
                    byte[] canonical = roundtrip(kind, bytes);
                    assertEquals(row.get("canonical").getAsString(), new String(canonical, StandardCharsets.UTF_8));
                    assertArrayEquals(canonical, roundtrip(kind, canonical));
                    if (row.has("negotiation")) {
                        assertEquals(row.get("negotiation").getAsInt(), ServerFeatureProtocol.negotiate(ServerFeatureCodec.decodeRequest(bytes)));
                    }
                    if (row.has("negotiationError")) {
                        var request = ServerFeatureCodec.decodeRequest(bytes);
                        ProtocolException failure = assertThrows(ProtocolException.class, () -> ServerFeatureProtocol.negotiate(request));
                        assertEquals(row.get("negotiationError").getAsString(), failure.code().wire());
                    }
                }
            });
        });
    }

    private static byte[] roundtrip(String kind, byte[] input) {
        return switch (kind) {
            case "request" -> ServerFeatureCodec.encodeRequest(ServerFeatureCodec.decodeRequest(input));
            case "manifest" -> ServerFeatureCodec.encodeManifest(ServerFeatureCodec.decodeManifest(input));
            case "failure" -> ServerFeatureCodec.encodeFailure(ServerFeatureCodec.decodeFailure(input));
            default -> throw new AssertionError("Unknown trusted fixture kind");
        };
    }
}
