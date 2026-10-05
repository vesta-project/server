package com.vesta.wrapper.protocol;

import com.vesta.wrapper.protocol.ServerFeatureProtocol.*;
import org.junit.jupiter.api.Test;

import java.nio.charset.StandardCharsets;
import java.util.Arrays;
import java.util.List;

import static com.vesta.wrapper.protocol.ServerFeatureProtocol.*;
import static com.vesta.wrapper.protocol.ServerFeatureProtocol.FeatureId.*;
import static com.vesta.wrapper.protocol.ServerFeatureProtocolTest.*;
import static org.junit.jupiter.api.Assertions.*;

class ServerFeatureCodecTest {
    @Test void actualJavaOutputEqualsIndependentCanonicalGoldenBytes() throws Exception {
        assertArrayEquals(ServerFeatureConformanceTest.fixture("request.json"), ServerFeatureCodec.encodeRequest(new DiscoveryRequest(List.of(1,2))));
        var manifest = new Manifest(1, ID, List.of(Feature.supported(GAME_START), Feature.unsupported(GAME_STOP,"unsupported_stop"),
                Feature.supported(GAME_FORCE_KILL), Feature.supported(TASKS_REPLAY), Feature.unsupported(GAME_READINESS,"not_implemented"),
                Feature.unsupported(SUPERVISOR_CONTINUITY,"not_implemented")));
        assertArrayEquals(ServerFeatureConformanceTest.fixture("manifest.json"), ServerFeatureCodec.encodeManifest(manifest));
        assertArrayEquals(ServerFeatureConformanceTest.fixture("failure.json"), ServerFeatureCodec.encodeFailure(new Failure(ErrorCode.UNSUPPORTED_PROTOCOL)));
    }

    @Test void capIsInclusiveAndCountsCompleteBytesIncludingWhitespace() {
        byte[] raw = ServerFeatureCodec.encodeRequest(new DiscoveryRequest(List.of(1)));
        byte[] exact = new byte[MAX_BYTES]; Arrays.fill(exact,(byte)' ');
        System.arraycopy(raw,0,exact,exact.length-raw.length,raw.length);
        assertEquals(new DiscoveryRequest(List.of(1)),ServerFeatureCodec.decodeRequest(exact));
        failure(ErrorCode.MESSAGE_TOO_LARGE, () -> ServerFeatureCodec.decodeRequest(Arrays.copyOf(exact,MAX_BYTES+1)));
        // Fewer UTF-16 characters than the cap, but more UTF-8 bytes: reject before parsing.
        String text = " ".repeat(16_000) + "é".repeat(200);
        assertTrue(text.length() < MAX_BYTES); assertTrue(text.getBytes(StandardCharsets.UTF_8).length > MAX_BYTES);
        failure(ErrorCode.MESSAGE_TOO_LARGE, () -> ServerFeatureCodec.decodeRequest(text.getBytes(StandardCharsets.UTF_8)));
    }

    @Test void callerCannotPassNullToAnyCodec() {
        failure(ErrorCode.INVALID_MESSAGE, () -> ServerFeatureCodec.decodeRequest(null));
        failure(ErrorCode.INVALID_MESSAGE, () -> ServerFeatureCodec.decodeManifest(null));
        failure(ErrorCode.INVALID_MESSAGE, () -> ServerFeatureCodec.decodeFailure(null));
        failure(ErrorCode.INVALID_MESSAGE, () -> ServerFeatureCodec.encodeRequest(null));
        failure(ErrorCode.INVALID_MESSAGE, () -> ServerFeatureCodec.encodeManifest(null));
        failure(ErrorCode.INVALID_MESSAGE, () -> ServerFeatureCodec.encodeFailure(null));
    }

    @Test void boundedEncodingDoesNotTruncateUnsupportedEntriesAndEachArrayIsNew() {
        var manifest = new Manifest(1,ID,List.of(Feature.unsupported(GAME_START,"not_configured"), Feature.unsupported(GAME_STOP,"not_configured")));
        byte[] encoded = ServerFeatureCodec.encodeManifest(manifest);
        assertEquals(manifest,ServerFeatureCodec.decodeManifest(encoded));
        assertTrue(encoded.length <= MAX_BYTES);
        encoded[0] = 0;
        assertEquals(manifest,ServerFeatureCodec.decodeManifest(ServerFeatureCodec.encodeManifest(manifest)));
    }

    @Test void arbitraryValuesAreNotEchoedInErrorsOrCauses() {
        byte[] malicious = ("{\"protocol\":\""+PROTOCOL+"\",\"versions\":[1],\"password\":\"SUPER_SECRET\"}").getBytes(StandardCharsets.UTF_8);
        ProtocolException e = assertThrows(ProtocolException.class, () -> ServerFeatureCodec.decodeRequest(malicious));
        assertFalse(e.toString().contains("SUPER_SECRET")); assertFalse(e.toString().contains("password")); assertNull(e.getCause());
        var encodedFailure = ServerFeatureCodec.encodeFailure(new Failure(e.code()));
        assertFalse(new String(encodedFailure,StandardCharsets.UTF_8).contains("SUPER_SECRET"));
        assertEquals(new Failure(ErrorCode.INVALID_MESSAGE),ServerFeatureCodec.decodeFailure(encodedFailure));
    }

    @Test void wrongSelectedVersionCannotHideTrailingMalformedContent() {
        String input = "{\"protocol\":\""+PROTOCOL+"\",\"version\":2,\"server_id\":\""+ID+"\",\"features\":[]} {}";
        failure(ErrorCode.INVALID_MESSAGE, () -> ServerFeatureCodec.decodeManifest(input.getBytes(StandardCharsets.UTF_8)));
    }
}
