package com.vesta.wrapper.protocol;

import java.util.ArrayList;
import java.util.Comparator;
import java.util.HashSet;
import java.util.List;
import java.util.Objects;
import java.util.regex.Pattern;

/** Draft, transport-neutral implementation discovery. Nothing here authenticates or grants authority. */
public final class ServerFeatureProtocol {
    public static final String PROTOCOL = "vesta.server-capabilities";
    public static final int VERSION = 1;
    public static final int MAX_BYTES = 16_384;
    public static final int MAX_VERSIONS = 8;
    public static final int MAX_VERSION = 65_535;
    public static final int MAX_FEATURES = 64;
    public static final int MAX_ID_CHARS = 64;
    private static final Pattern ID = Pattern.compile("[a-z][a-z0-9]*(?:[._-][a-z0-9]+)*");
    private static final Pattern REASON = Pattern.compile("[a-z][a-z0-9_]*");
    private static final Pattern UUID = Pattern.compile("[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}");

    private ServerFeatureProtocol() { }

    /** Known descriptive features, deliberately separate from operation dispatch and permission IDs. */
    public enum FeatureId {
        GAME_START("game.start"), GAME_STOP("game.stop"), GAME_FORCE_KILL("game.force_kill"),
        TASKS_REPLAY("tasks.replay"), GAME_READINESS("game.readiness"), SUPERVISOR_CONTINUITY("supervisor.continuity");

        private final String id;
        FeatureId(String id) { this.id = id; }
        public String id() { return id; }
    }

    public enum Support {
        SUPPORTED("supported"), UNSUPPORTED("unsupported");
        private final String wire;
        Support(String wire) { this.wire = wire; }
        public String wire() { return wire; }
        public static Support fromWire(String wire) {
            for (Support value : values()) if (value.wire.equals(wire)) return value;
            throw invalid();
        }
    }

    public enum ErrorCode {
        INVALID_MESSAGE("invalid_message"), MESSAGE_TOO_LARGE("message_too_large"),
        UNSUPPORTED_PROTOCOL("unsupported_protocol"), UNEXPECTED_RESPONSE("unexpected_response");
        private final String wire;
        ErrorCode(String wire) { this.wire = wire; }
        public String wire() { return wire; }
        public static ErrorCode fromWire(String wire) {
            for (ErrorCode code : values()) if (code.wire.equals(wire)) return code;
            throw invalid();
        }
    }

    /** Fixed messages, with no parser/input-bearing cause. */
    public static final class ProtocolException extends RuntimeException {
        private final ErrorCode code;
        public ProtocolException(ErrorCode code) {
            super("Server feature protocol failed: " + Objects.requireNonNull(code).wire(), null);
            this.code = code;
        }
        public ErrorCode code() { return code; }
    }

    public record DiscoveryRequest(List<Integer> versions) {
        public DiscoveryRequest {
            if (versions == null || versions.isEmpty() || versions.size() > MAX_VERSIONS) throw invalid();
            var seen = new HashSet<Integer>();
            var copy = new ArrayList<Integer>();
            for (Integer version : versions) {
                if (version == null || version < 1 || version > MAX_VERSION || !seen.add(version)) throw invalid();
                copy.add(version);
            }
            copy.sort(Comparator.reverseOrder());
            versions = List.copyOf(copy);
        }
    }

    /** Unknown valid IDs/reasons are descriptive data, never policy or executable instructions. */
    public record Feature(String id, Support support, String reason) {
        public Feature {
            if (!token(id, ID) || support == null) throw invalid();
            if (support == Support.SUPPORTED ? reason != null : !token(reason, REASON)) throw invalid();
        }
        public static Feature supported(FeatureId id) {
            if (id == null) throw invalid();
            return new Feature(id.id(), Support.SUPPORTED, null);
        }
        public static Feature unsupported(FeatureId id, String reason) {
            if (id == null) throw invalid();
            return new Feature(id.id(), Support.UNSUPPORTED, reason);
        }
        @Override public String toString() { return "Feature[support=" + support.wire() + "]"; }
    }

    public record Manifest(int version, String serverId, List<Feature> features) {
        public Manifest {
            if (version < 1 || version > MAX_VERSION) throw invalid();
            if (version != VERSION) throw new ProtocolException(ErrorCode.UNSUPPORTED_PROTOCOL);
            requireServerId(serverId);
            if (features == null || features.size() > MAX_FEATURES) throw invalid();
            var seen = new HashSet<String>();
            var copy = new ArrayList<Feature>();
            for (Feature feature : features) {
                if (feature == null || !seen.add(feature.id())) throw invalid();
                copy.add(feature);
            }
            copy.sort(Comparator.comparing(Feature::id));
            features = List.copyOf(copy);
        }
        /** Missing/unknown features cannot enable a known action. This is not an authorization check. */
        public boolean supports(FeatureId id) {
            if (id == null) throw invalid();
            return features.stream().anyMatch(f -> f.id().equals(id.id()) && f.support() == Support.SUPPORTED);
        }
        @Override public String toString() { return "Manifest[version=" + version + ", features=" + features.size() + "]"; }
    }

    /** Usable before successful negotiation; contains neither selected version nor server identity. */
    public record Failure(ErrorCode error) {
        public Failure { if (error == null) throw invalid(); }
    }

    /** Highest mutually implemented version. Discovery and task-event versions are separate domains. */
    public static int negotiate(DiscoveryRequest request) {
        if (request == null) throw invalid();
        if (request.versions().contains(VERSION)) return VERSION;
        throw new ProtocolException(ErrorCode.UNSUPPORTED_PROTOCOL);
    }

    /** Consistency only: the caller must obtain expectedServerId from its authenticated context. */
    public static void checkResponse(DiscoveryRequest request, String expectedServerId, Manifest manifest) {
        if (request == null || manifest == null) throw invalid();
        requireServerId(expectedServerId);
        if (!request.versions().contains(manifest.version()) || !expectedServerId.equals(manifest.serverId())) {
            throw new ProtocolException(ErrorCode.UNEXPECTED_RESPONSE);
        }
    }

    private static boolean token(String value, Pattern pattern) {
        return value != null && value.length() <= MAX_ID_CHARS && pattern.matcher(value).matches();
    }

    private static void requireServerId(String id) {
        if (id == null || !UUID.matcher(id).matches()) throw invalid();
    }

    static ProtocolException invalid() { return new ProtocolException(ErrorCode.INVALID_MESSAGE); }
}
