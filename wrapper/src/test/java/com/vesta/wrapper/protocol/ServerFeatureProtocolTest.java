package com.vesta.wrapper.protocol;

import com.vesta.wrapper.protocol.ServerFeatureProtocol.*;
import org.junit.jupiter.api.DynamicTest;
import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.TestFactory;
import org.junit.jupiter.api.function.Executable;

import java.util.ArrayList;
import java.util.Arrays;
import java.util.List;
import java.util.stream.IntStream;
import java.util.stream.Stream;

import static com.vesta.wrapper.protocol.ServerFeatureProtocol.*;
import static com.vesta.wrapper.protocol.ServerFeatureProtocol.FeatureId.*;
import static org.junit.jupiter.api.Assertions.*;

class ServerFeatureProtocolTest {
    static final String ID = "12345678-1234-5678-9abc-123456789abc";

    @Test void requestCopiesSortsAndNeverFallsBackToAnUnofferedVersion() {
        var input = new ArrayList<>(List.of(1, 65535, 2));
        var request = new DiscoveryRequest(input);
        input.clear();
        assertEquals(List.of(65535, 2, 1), request.versions());
        assertThrows(UnsupportedOperationException.class, () -> request.versions().add(3));
        assertEquals(1, negotiate(request));
        assertEquals(new DiscoveryRequest(List.of(2, 1, 65535)), request);
        failure(ErrorCode.UNSUPPORTED_PROTOCOL, () -> negotiate(new DiscoveryRequest(List.of(2))));
    }

    @Test void manifestCopiesSortsAndOnlyExactKnownIdsAreQueryable() {
        var input = new ArrayList<>(List.of(Feature.supported(GAME_START), Feature.supported(GAME_FORCE_KILL)));
        var manifest = new Manifest(1, ID, input);
        input.clear();
        assertEquals(List.of("game.force_kill", "game.start"), manifest.features().stream().map(Feature::id).toList());
        assertThrows(UnsupportedOperationException.class, () -> manifest.features().clear());
        assertTrue(manifest.supports(GAME_START)); assertFalse(manifest.supports(GAME_STOP));
        assertEquals(manifest, new Manifest(1, ID, List.of(Feature.supported(GAME_FORCE_KILL), Feature.supported(GAME_START))));
        assertEquals(manifest.hashCode(), new Manifest(1, ID, manifest.features()).hashCode());
    }

    @Test void familiarPermissionStringsPrefixesAndUnknownIdsAreInert() {
        var manifest = new Manifest(1, ID, List.of(new Feature("lifecycle.start", Support.SUPPORTED, null),
                new Feature("security.manage", Support.SUPPORTED, null), new Feature("game.start.extra", Support.SUPPORTED, null),
                new Feature("constructor", Support.SUPPORTED, null)));
        for (FeatureId known : FeatureId.values()) assertFalse(manifest.supports(known));
        assertEquals(4, manifest.features().size());
    }

    @Test void unknownReasonIsPreservedWithoutChangingUnsupportedStatus() {
        var feature = Feature.unsupported(GAME_STOP, "future_reason");
        assertEquals("future_reason", feature.reason());
        assertFalse(new Manifest(1, ID, List.of(feature)).supports(GAME_STOP));
    }

    @Test void clientChecksOfferedVersionAndExpectedServerButThisIsNotAuthentication() {
        var manifest = new Manifest(1, ID, List.of());
        checkResponse(new DiscoveryRequest(List.of(2,1)), ID, manifest);
        failure(ErrorCode.UNEXPECTED_RESPONSE, () -> checkResponse(new DiscoveryRequest(List.of(2)), ID, manifest));
        failure(ErrorCode.UNEXPECTED_RESPONSE, () -> checkResponse(new DiscoveryRequest(List.of(1)), "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa", manifest));
        failure(ErrorCode.INVALID_MESSAGE, () -> checkResponse(new DiscoveryRequest(List.of(1)), "secret", manifest));
    }

    @Test void exactProgrammaticMaximaAreAccepted() {
        var versions = new DiscoveryRequest(List.of(1,2,3,4,5,6,7,65535));
        assertEquals(MAX_VERSIONS, versions.versions().size());
        var features = IntStream.range(0,64).mapToObj(i -> new Feature("x" + String.format("%02d", i) + "a".repeat(61),
                Support.UNSUPPORTED, "r".repeat(64))).toList();
        var manifest = new Manifest(1, ID, features);
        assertEquals(MAX_FEATURES, manifest.features().size());
        assertTrue(ServerFeatureCodec.encodeManifest(manifest).length <= MAX_BYTES);
        assertTrue(ServerFeatureCodec.encodeRequest(versions).length <= MAX_BYTES);
    }

    @Test void allFixedErrorCodesRoundtripAndToStringDoesNotEchoDeclarationValues() {
        for (ErrorCode code : ErrorCode.values()) {
            var failure = new Failure(code);
            assertEquals(failure, ServerFeatureCodec.decodeFailure(ServerFeatureCodec.encodeFailure(failure)));
        }
        assertFalse(new Manifest(1, ID, List.of(new Feature("secret_value", Support.UNSUPPORTED, "secret_reason"))).toString().contains("secret"));
        assertFalse(new Feature("secret_value", Support.UNSUPPORTED, "secret_reason").toString().contains("secret"));
    }

    private record Bad(String name, Executable work) { }
    @TestFactory Stream<DynamicTest> programmaticValidationCannotBypassWireValidation() {
        var invalid = new ArrayList<Bad>(List.of(
                new Bad("null request list", () -> new DiscoveryRequest(null)),
                new Bad("empty versions", () -> new DiscoveryRequest(List.of())),
                new Bad("too many versions", () -> new DiscoveryRequest(IntStream.rangeClosed(1,9).boxed().toList())),
                new Bad("duplicate versions", () -> new DiscoveryRequest(List.of(1,1))),
                new Bad("null version", () -> new DiscoveryRequest(Arrays.asList(1,null))),
                new Bad("null features", () -> new Manifest(1,ID,null)),
                new Bad("null feature", () -> new Manifest(1,ID,Arrays.asList((Feature)null))),
                new Bad("too many features", () -> new Manifest(1,ID,IntStream.range(0,65).mapToObj(i -> new Feature("x_"+i,Support.SUPPORTED,null)).toList())),
                new Bad("duplicate conflicting features", () -> new Manifest(1,ID,List.of(Feature.supported(GAME_START),Feature.unsupported(GAME_START,"future_reason")))),
                new Bad("null support", () -> new Feature("game.start",null,null)),
                new Bad("supported with reason", () -> new Feature("game.start",Support.SUPPORTED,"secret")),
                new Bad("unsupported null reason", () -> new Feature("game.start",Support.UNSUPPORTED,null)),
                new Bad("oversize reason", () -> new Feature("game.start",Support.UNSUPPORTED,"r".repeat(65))),
                new Bad("null failure", () -> new Failure(null)),
                new Bad("null known feature", () -> Feature.supported(null)),
                new Bad("null known unsupported feature", () -> Feature.unsupported(null,"future_reason")),
                new Bad("null negotiate", () -> negotiate(null)),
                new Bad("null response request", () -> checkResponse(null,ID,new Manifest(1,ID,List.of()))),
                new Bad("null response manifest", () -> checkResponse(new DiscoveryRequest(List.of(1)),ID,null)),
                new Bad("null support query", () -> new Manifest(1,ID,List.of()).supports(null)),
                new Bad("unknown support wire", () -> Support.fromWire("secret")),
                new Bad("unknown error wire", () -> ErrorCode.fromWire("secret"))));
        for (int version : new int[]{0,-1,65536,Integer.MAX_VALUE}) invalid.add(new Bad("invalid version "+version, () -> new DiscoveryRequest(List.of(version))));
        for (int version : new int[]{0,-1,65536,Integer.MAX_VALUE}) invalid.add(new Bad("invalid selected version "+version, () -> new Manifest(version,ID,List.of())));
        for (String id : Arrays.asList(null,"",ID.toUpperCase(),"secret",ID+"\n")) invalid.add(new Bad("invalid server identity", () -> new Manifest(1,id,List.of())));
        for (String id : Arrays.asList(null,"","game.*","game..start","game.start\n","a".repeat(65))) invalid.add(new Bad("invalid feature ID", () -> new Feature(id,Support.SUPPORTED,null)));
        for (String reason : List.of("","BAD","bad.reason","bad reason","reason\n")) invalid.add(new Bad("invalid reason", () -> new Feature("game.stop",Support.UNSUPPORTED,reason)));
        Stream<DynamicTest> bad = invalid.stream().map(value -> DynamicTest.dynamicTest(value.name(), () -> failure(ErrorCode.INVALID_MESSAGE,value.work())));
        return Stream.concat(bad, Stream.of(DynamicTest.dynamicTest("unknown manifest version", () -> failure(ErrorCode.UNSUPPORTED_PROTOCOL, () -> new Manifest(2,ID,List.of())))));
    }

    static void failure(ErrorCode code, Executable work) {
        ProtocolException e = assertThrows(ProtocolException.class, work);
        assertEquals(code,e.code()); assertNull(e.getCause());
        assertEquals("Server feature protocol failed: "+code.wire(),e.getMessage());
    }
}
