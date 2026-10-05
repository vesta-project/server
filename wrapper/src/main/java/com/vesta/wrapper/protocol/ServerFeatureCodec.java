package com.vesta.wrapper.protocol;

import com.google.gson.Strictness;
import com.google.gson.stream.JsonReader;
import com.google.gson.stream.JsonToken;
import com.google.gson.stream.JsonWriter;
import com.vesta.wrapper.protocol.ServerFeatureProtocol.DiscoveryRequest;
import com.vesta.wrapper.protocol.ServerFeatureProtocol.ErrorCode;
import com.vesta.wrapper.protocol.ServerFeatureProtocol.Failure;
import com.vesta.wrapper.protocol.ServerFeatureProtocol.Feature;
import com.vesta.wrapper.protocol.ServerFeatureProtocol.Manifest;
import com.vesta.wrapper.protocol.ServerFeatureProtocol.ProtocolException;
import com.vesta.wrapper.protocol.ServerFeatureProtocol.Support;

import java.io.IOException;
import java.io.StringReader;
import java.io.StringWriter;
import java.nio.ByteBuffer;
import java.nio.charset.CodingErrorAction;
import java.nio.charset.StandardCharsets;
import java.util.ArrayList;
import java.util.HashSet;
import java.util.List;
import java.util.Set;

import static com.vesta.wrapper.protocol.ServerFeatureProtocol.*;

/** Byte-bounded strict draft-v1 codec. Decoded messages are inert observations, not trusted policy. */
public final class ServerFeatureCodec {
    private static final Set<String> REQUEST_KEYS = Set.of("protocol", "versions");
    private static final Set<String> MANIFEST_KEYS = Set.of("protocol", "version", "server_id", "features");
    private static final Set<String> FEATURE_KEYS = Set.of("id", "support", "reason");
    private static final Set<String> ERROR_KEYS = Set.of("protocol", "error");

    private ServerFeatureCodec() { }

    public static DiscoveryRequest decodeRequest(byte[] bytes) {
        return read(bytes, reader -> {
            Set<String> seen = new HashSet<>();
            String protocol = null;
            List<Integer> versions = null;
            reader.beginObject();
            while (reader.hasNext()) {
                switch (member(reader, seen, REQUEST_KEYS)) {
                    case "protocol" -> protocol = string(reader);
                    case "versions" -> {
                        versions = new ArrayList<>();
                        reader.beginArray();
                        while (reader.hasNext()) {
                            if (versions.size() == MAX_VERSIONS) throw invalid();
                            versions.add(version(reader));
                        }
                        reader.endArray();
                    }
                    default -> throw invalid();
                }
            }
            reader.endObject();
            required(reader, seen, REQUEST_KEYS, protocol);
            return new DiscoveryRequest(versions);
        });
    }

    public static Manifest decodeManifest(byte[] bytes) {
        return read(bytes, reader -> {
            Set<String> seen = new HashSet<>();
            String protocol = null, serverId = null;
            int version = 0;
            List<Feature> features = null;
            reader.beginObject();
            while (reader.hasNext()) {
                switch (member(reader, seen, MANIFEST_KEYS)) {
                    case "protocol" -> protocol = string(reader);
                    case "version" -> version = version(reader);
                    case "server_id" -> serverId = string(reader);
                    case "features" -> {
                        features = new ArrayList<>();
                        reader.beginArray();
                        while (reader.hasNext()) {
                            if (features.size() == MAX_FEATURES) throw invalid();
                            features.add(feature(reader));
                        }
                        reader.endArray();
                    }
                    default -> throw invalid();
                }
            }
            reader.endObject();
            required(reader, seen, MANIFEST_KEYS, protocol);
            return new Manifest(version, serverId, features);
        });
    }

    public static Failure decodeFailure(byte[] bytes) {
        return read(bytes, reader -> {
            Set<String> seen = new HashSet<>();
            String protocol = null, error = null;
            reader.beginObject();
            while (reader.hasNext()) {
                switch (member(reader, seen, ERROR_KEYS)) {
                    case "protocol" -> protocol = string(reader);
                    case "error" -> error = string(reader);
                    default -> throw invalid();
                }
            }
            reader.endObject();
            required(reader, seen, ERROR_KEYS, protocol);
            return new Failure(ErrorCode.fromWire(error));
        });
    }

    private static Feature feature(JsonReader reader) throws IOException {
        Set<String> seen = new HashSet<>();
        String id = null, support = null, reason = null;
        reader.beginObject();
        while (reader.hasNext()) {
            switch (member(reader, seen, FEATURE_KEYS)) {
                case "id" -> id = string(reader);
                case "support" -> support = string(reader);
                case "reason" -> {
                    if (reader.peek() == JsonToken.NULL) reader.nextNull();
                    else reason = string(reader);
                }
                default -> throw invalid();
            }
        }
        reader.endObject();
        if (!seen.equals(FEATURE_KEYS)) throw invalid();
        return new Feature(id, Support.fromWire(support), reason);
    }

    private static String member(JsonReader reader, Set<String> seen, Set<String> allowed) throws IOException {
        String name = reader.nextName();
        if (!allowed.contains(name) || !seen.add(name)) throw invalid();
        return name;
    }

    private static void required(JsonReader reader, Set<String> seen, Set<String> keys, String protocol) throws IOException {
        if (!seen.equals(keys) || !PROTOCOL.equals(protocol) || reader.peek() != JsonToken.END_DOCUMENT) throw invalid();
    }

    private static String string(JsonReader reader) throws IOException {
        if (reader.peek() != JsonToken.STRING) throw invalid();
        String value = reader.nextString();
        if (value.length() > MAX_ID_CHARS) throw invalid();
        return value;
    }

    private static int version(JsonReader reader) throws IOException {
        if (reader.peek() != JsonToken.NUMBER) throw invalid();
        String text = reader.nextString();
        if (!text.matches("[1-9][0-9]{0,4}")) throw invalid();
        int value = Integer.parseInt(text);
        if (value > MAX_VERSION) throw invalid();
        return value;
    }

    @FunctionalInterface private interface Decoder<T> { T read(JsonReader reader) throws IOException; }
    @FunctionalInterface private interface Encoder { void write(JsonWriter writer) throws IOException; }

    private static <T> T read(byte[] bytes, Decoder<T> decode) {
        if (bytes == null) throw invalid();
        if (bytes.length > MAX_BYTES) throw new ProtocolException(ErrorCode.MESSAGE_TOO_LARGE);
        try {
            String text = StandardCharsets.UTF_8.newDecoder().onMalformedInput(CodingErrorAction.REPORT)
                    .onUnmappableCharacter(CodingErrorAction.REPORT).decode(ByteBuffer.wrap(bytes)).toString();
            if (!text.isEmpty() && text.charAt(0) == 0xFEFF) throw invalid();
            try (var reader = new JsonReader(new StringReader(text))) {
                reader.setStrictness(Strictness.STRICT);
                T result = decode.read(reader);
                if (reader.peek() != JsonToken.END_DOCUMENT) throw invalid();
                return result;
            }
        } catch (ProtocolException e) {
            throw e;
        } catch (IOException | RuntimeException e) {
            throw invalid();
        }
    }

    public static byte[] encodeRequest(DiscoveryRequest request) {
        if (request == null) throw invalid();
        return write(writer -> {
            header(writer);
            writer.name("versions").beginArray();
            for (int version : request.versions()) writer.value(version);
            writer.endArray().endObject();
        });
    }

    public static byte[] encodeManifest(Manifest manifest) {
        if (manifest == null) throw invalid();
        return write(writer -> {
            header(writer);
            writer.name("version").value(manifest.version());
            writer.name("server_id").value(manifest.serverId());
            writer.name("features").beginArray();
            for (Feature feature : manifest.features()) {
                writer.beginObject().name("id").value(feature.id()).name("support").value(feature.support().wire())
                        .name("reason").value(feature.reason()).endObject();
            }
            writer.endArray().endObject();
        });
    }

    public static byte[] encodeFailure(Failure failure) {
        if (failure == null) throw invalid();
        return write(writer -> { header(writer); writer.name("error").value(failure.error().wire()).endObject(); });
    }

    private static void header(JsonWriter writer) throws IOException {
        writer.beginObject().name("protocol").value(PROTOCOL);
    }

    private static byte[] write(Encoder encode) {
        try {
            StringWriter out = new StringWriter();
            try (var writer = new JsonWriter(out)) {
                writer.setSerializeNulls(true);
                encode.write(writer);
            }
            byte[] bytes = out.toString().getBytes(StandardCharsets.UTF_8);
            if (bytes.length > MAX_BYTES) throw new ProtocolException(ErrorCode.MESSAGE_TOO_LARGE);
            return bytes;
        } catch (ProtocolException e) {
            throw e;
        } catch (IOException | RuntimeException e) {
            throw invalid();
        }
    }
}
