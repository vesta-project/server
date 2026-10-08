import com.google.gson.Gson;
import com.google.gson.GsonBuilder;
import com.google.gson.JsonArray;
import com.google.gson.JsonNull;
import com.google.gson.JsonObject;

import java.io.InputStream;
import java.lang.reflect.Constructor;
import java.lang.reflect.Method;
import java.net.URI;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.security.MessageDigest;
import java.util.ArrayList;
import java.util.AbstractMap;
import java.util.HexFormat;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Locale;
import java.util.Map;
import java.util.Properties;
import java.util.TreeMap;

/**
 * Independent source-file oracle for frozen Vesta audit-v1 hash vectors.
 * Run with the repository-pinned Gson JAR on the class path; expected values are
 * emitted by this Java program and are not derived from the Rust implementation.
 */
public final class AuditHashOracle {
    private static final Gson GSON = new Gson();
    private static final Gson PRETTY = new GsonBuilder().serializeNulls().setPrettyPrinting().create();
    private static final String DOMAIN = "vesta-audit-v1";
    private static final String REFERENCE = "wrapper/src/main/java/com/vesta/wrapper/audit/AuditLog.java:144-162";
    private static final String METADATA_REFERENCE =
            "wrapper/src/main/java/com/vesta/wrapper/audit/AuditRecord.java:22-55";

    private record Vector(String id, String previousCaseId, String prevHash, String serverId,
                         String principalId, String deviceId, String source, String action,
                         String target, String outcome, String operationId, long atMs,
                         List<Map.Entry<String, String>> metadataInput) {
    }

    private record Computed(JsonObject json, String hash) {
    }

    private record GsonProvenance(String version, String coordinate, String sha256) {
    }

    public static void main(String[] args) throws Exception {
        boolean verifyReference = args.length == 1 && "--verify-reference".equals(args[0]);
        if (args.length > 1 || (args.length == 1 && !verifyReference)) {
            throw new IllegalArgumentException("Usage: java -cp <gson.jar[:production-classes]> scripts/AuditHashOracle.java [--verify-reference]");
        }
        GsonProvenance gson = inspectGson();
        List<Vector> vectors = vectors();
        Map<String, String> priorHashes = new LinkedHashMap<>();
        JsonArray outputVectors = new JsonArray();
        ReferenceVerifier verifier = verifyReference ? new ReferenceVerifier() : null;
        for (Vector vector : vectors) {
            Computed computed = compute(vector, gson.version(), priorHashes);
            if (verifier != null) {
                verifier.verify(vector, computed);
            }
            outputVectors.add(computed.json());
            priorHashes.put(vector.id(), computed.hash());
        }

        JsonObject provenance = new JsonObject();
        provenance.addProperty("oracle", "scripts/AuditHashOracle.java (Java source-file execution)");
        provenance.addProperty("gson_version", gson.version());
        provenance.addProperty("gson_coordinate", gson.coordinate());
        provenance.addProperty("gson_sha256", gson.sha256());
        provenance.addProperty("gson_version_evidence", "META-INF/maven/com.google.code.gson/gson/pom.properties loaded through the Gson classloader and checked against the pinned coordinate");
        provenance.addProperty("gson_convention", "new Gson() defaults; compact JSON, HTML-safe escaping enabled");
        provenance.addProperty("field_array_convention", "JSON array [domain, prev_hash, server_id, principal_id, device_id, source, action, target, outcome, operation_id, at_ms, metadata_json]");
        provenance.addProperty("metadata_convention", "AuditRecord normalizes null metadata to {}, copies entries into natural-order TreeMap<String,String>, and normalizes null values to empty strings");
        provenance.addProperty("string_ordering", "Java String natural ordering compares UTF-16 code units");
        provenance.addProperty("hash_convention", "SHA-256 over UTF-8 bytes of Gson compact serialization of the field array; lowercase hexadecimal");
        provenance.addProperty("reference", REFERENCE);
        provenance.addProperty("metadata_reference", METADATA_REFERENCE);
        provenance.addProperty("gson_pin", "core/build.gradle.kts and wrapper/build.gradle.kts: com.google.code.gson:gson:2.11.0");

        JsonObject document = new JsonObject();
        document.add("provenance", provenance);
        document.add("vectors", outputVectors);
        System.out.write(PRETTY.toJson(document).getBytes(StandardCharsets.UTF_8));
        System.out.write('\n');
        System.out.flush();
    }

    private static GsonProvenance inspectGson() throws Exception {
        Properties pom = new Properties();
        String resource = "META-INF/maven/com.google.code.gson/gson/pom.properties";
        try (InputStream input = Gson.class.getClassLoader().getResourceAsStream(resource)) {
            if (input == null) {
                throw new IllegalStateException("Missing loaded Gson Maven metadata: " + resource);
            }
            pom.load(input);
        }
        String groupId = pom.getProperty("groupId");
        String artifactId = pom.getProperty("artifactId");
        String version = pom.getProperty("version");
        if (!"com.google.code.gson".equals(groupId) || !"gson".equals(artifactId)
                || !"2.11.0".equals(version)) {
            throw new IllegalStateException("Loaded Gson Maven metadata does not match pinned coordinate: "
                    + groupId + ":" + artifactId + ":" + version);
        }
        String packageVersion = Gson.class.getPackage().getImplementationVersion();
        if (packageVersion != null && !version.equals(packageVersion)) {
            throw new IllegalStateException("Gson package version disagrees with loaded Maven metadata: "
                    + packageVersion + " != " + version);
        }
        if (Gson.class.getProtectionDomain().getCodeSource() == null) {
            throw new IllegalStateException("Cannot locate loaded Gson JAR for content hashing");
        }
        URI location = Gson.class.getProtectionDomain().getCodeSource().getLocation().toURI();
        Path jar = Path.of(location);
        if (!Files.isRegularFile(jar)) {
            throw new IllegalStateException("Loaded Gson code source is not a regular JAR file");
        }
        String sha256 = HexFormat.of().formatHex(MessageDigest.getInstance("SHA-256")
                .digest(Files.readAllBytes(jar)));
        return new GsonProvenance(version, groupId + ":" + artifactId + ":" + version, sha256);
    }

    private static final class ReferenceVerifier {
        private final Constructor<?> actorConstructor;
        private final Constructor<?> recordConstructor;
        private final Method metadataAccessor;
        private final Method chainHash;

        private ReferenceVerifier() throws ReflectiveOperationException {
            ClassLoader loader = AuditHashOracle.class.getClassLoader();
            Class<?> actorClass = Class.forName("com.vesta.wrapper.audit.AuditActor", true, loader);
            Class<?> recordClass = Class.forName("com.vesta.wrapper.audit.AuditRecord", true, loader);
            Class<?> logClass = Class.forName("com.vesta.wrapper.audit.AuditLog", true, loader);
            actorConstructor = actorClass.getConstructor(String.class, String.class, String.class);
            recordConstructor = recordClass.getConstructor(actorClass, String.class, String.class,
                    String.class, String.class, Map.class);
            metadataAccessor = recordClass.getMethod("metadata");
            chainHash = logClass.getDeclaredMethod("chainHash", String.class, String.class,
                    String.class, String.class, String.class, String.class, String.class,
                    String.class, String.class, long.class, String.class);
            chainHash.setAccessible(true);
        }

        private void verify(Vector vector, Computed expected) throws ReflectiveOperationException {
            Object actor = actorConstructor.newInstance(vector.principalId(), vector.deviceId(), vector.source());
            Map<String, String> inputMetadata = null;
            if (vector.metadataInput() != null) {
                inputMetadata = new LinkedHashMap<>();
                for (Map.Entry<String, String> entry : vector.metadataInput()) {
                    inputMetadata.put(entry.getKey(), entry.getValue());
                }
            }
            Object record = recordConstructor.newInstance(actor, vector.action(), vector.target(),
                    vector.outcome(), vector.operationId(), inputMetadata);
            @SuppressWarnings("unchecked")
            Map<String, String> normalizedMetadata = (Map<String, String>) metadataAccessor.invoke(record);
            String actualMetadataJson = GSON.toJson(normalizedMetadata);
            String expectedMetadataJson = expected.json().get("metadata_json").getAsString();
            if (!expectedMetadataJson.equals(actualMetadataJson)) {
                throw new IllegalStateException("AuditRecord metadata mismatch for " + vector.id()
                        + ": expected " + expectedMetadataJson + ", actual " + actualMetadataJson);
            }

            String prevHash = expected.json().get("prev_hash").getAsString();
            Object result = chainHash.invoke(null, prevHash, vector.serverId(), vector.principalId(),
                    vector.deviceId(), vector.source(), vector.action(), vector.target(),
                    vector.outcome(), vector.operationId(), vector.atMs(), actualMetadataJson);
            if (!(result instanceof String actualHash) || !expected.hash().equals(actualHash)) {
                throw new IllegalStateException("AuditLog.chainHash mismatch for " + vector.id()
                        + ": expected " + expected.hash() + ", actual " + result);
            }
        }
    }

    private static List<Vector> vectors() {
        List<Vector> out = new ArrayList<>();
        out.add(vector("original-java-domain", null, "0000000000000000000000000000000000000000000000000000000000000000", "server-main", "user-42", "device-a1",
                "https", "server.start", "server:world", "accepted", "op-100", 1735689600123L,
                entries("zeta", "last", "alpha", "first")));
        out.add(vector("null-operation-id", null, "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
                "server-main", "system", "local", "operation-engine", "server.stop", "server:world",
                "accepted", null, 0L, entries("reason", "maintenance")));
        out.add(vector("null-metadata-normalized-empty-object", null,
                "1111111111111111111111111111111111111111111111111111111111111111",
                "server-main", "user-7", "device-b2", "local-cli", "config.read", "config:server",
                "accepted", "op-null-metadata", 42L, null));
        out.add(vector("gson-html-safe-escaping", null,
                "2222222222222222222222222222222222222222222222222222222222222222",
                "server-main", "user-8", "device-c3", "https", "config.update", "config:motd",
                "accepted", "op-html", 1735689600999L, entries("markup", "<tag>&='quote' >")));
        out.add(vector("unicode-line-separators", null,
                "3333333333333333333333333333333333333333333333333333333333333333",
                "server-main", "user-9", "device-d4", "https", "config.read", "config:motd",
                "accepted", "op-lines", 1735689601000L,
                entries("separators", "before middle after")));
        out.add(vector("metadata-quotes-backslash-newline-controls", null,
                "4444444444444444444444444444444444444444444444444444444444444444",
                "server-main", "user-10", "device-e5", "local-cli", "operation.inspect", "operation:op-55",
                "accepted", "op-control-values", 1735689601001L,
                entries("text", "quote=\" backslash=\\ newline=\n tab=\t nul=\u0000 soh=\u0001 backspace=\b formfeed=\f return=\r")));
        out.add(vector("unicode-utf16-tree-order", null,
                "5555555555555555555555555555555555555555555555555555555555555555",
                "server-main", "user-11", "device-f6", "https", "config.read", "config:unicode",
                "accepted", "op-unicode", 1735689601002L,
                // Input is deliberately reversed relative to Java UTF-16 natural order.
                entries("", "BMP private-use", "𐀀", "supplementary linear-A", "café", "😀")));
        out.add(vector("negative-signed-millisecond", null,
                "6666666666666666666666666666666666666666666666666666666666666666",
                "server-main", "user-12", "device-g7", "https", "server.start", "server:world",
                "accepted", "op-negative-time", Long.MIN_VALUE, entries("epoch", "minimum signed millis")));
        out.add(vector("large-signed-millisecond", null,
                "7777777777777777777777777777777777777777777777777777777777777777",
                "server-main", "user-13", "device-h8", "local-cli", "server.stop", "server:world",
                "accepted", "op-large-time", Long.MAX_VALUE, entries("epoch", "maximum signed millis")));
        // A real chain relation: this vector's prev_hash is the computed hash of the first vector.
        out.add(vector("non-genesis-chained-entry", "original-java-domain", null,
                "server-main", "user-42", "device-a1", "https", "server.stop", "server:world",
                "accepted", "op-101", 1735689600124L, entries("phase", "second")));
        out.add(vector("null-metadata-value-normalized-empty-string", null,
                "8888888888888888888888888888888888888888888888888888888888888888",
                "server-main", "user-14", "device-i9", "https", "config.read", "config:optional",
                "accepted", "op-null-value", 1735689601003L, entries("optional", null)));
        return out;
    }

    private static Vector vector(String id, String previousCaseId, String prevHash, String serverId,
                                 String principalId, String deviceId, String source, String action,
                                 String target, String outcome, String operationId, long atMs,
                                 List<Map.Entry<String, String>> metadataInput) {
        return new Vector(id, previousCaseId, prevHash, serverId, principalId, deviceId, source,
                action, target, outcome, operationId, atMs, metadataInput);
    }

    private static List<Map.Entry<String, String>> entries(String... alternating) {
        if ((alternating.length & 1) != 0) {
            throw new IllegalArgumentException("metadata entries must be key/value pairs");
        }
        List<Map.Entry<String, String>> result = new ArrayList<>();
        for (int i = 0; i < alternating.length; i += 2) {
            result.add(new AbstractMap.SimpleImmutableEntry<>(alternating[i], alternating[i + 1]));
        }
        return result;
    }

    private static Computed compute(Vector v, String gsonVersion, Map<String, String> priorHashes) throws Exception {
        requireText("serverId", v.serverId(), 128);
        requireText("principalId", v.principalId(), 128);
        requireText("deviceId", v.deviceId(), 128);
        requireText("source", v.source(), 64);
        requireText("action", v.action(), 96);
        requireText("target", v.target(), 512);
        requireText("outcome", v.outcome(), 32);
        if (v.operationId() != null) {
            requireText("operationId", v.operationId(), 64);
        }

        TreeMap<String, String> metadata = new TreeMap<>();
        if (v.metadataInput() != null) {
            if (v.metadataInput().size() > 16) {
                throw new IllegalArgumentException("too many metadata entries in " + v.id());
            }
            for (Map.Entry<String, String> entry : v.metadataInput()) {
                String key = requireText("metadata key", entry.getKey(), 64);
                String lower = key.toLowerCase(Locale.ROOT);
                for (String forbidden : List.of("password", "secret", "token", "key", "credential", "cookie", "authorization", "command")) {
                    if (lower.contains(forbidden)) {
                        throw new IllegalArgumentException("forbidden metadata key in " + v.id() + ": " + key);
                    }
                }
                if (metadata.containsKey(key)) {
                    throw new IllegalArgumentException("duplicate metadata key in " + v.id() + ": " + key);
                }
                String value = entry.getValue() == null ? "" : entry.getValue();
                if (value.length() > 256) {
                    throw new IllegalArgumentException("metadata value too long in " + v.id());
                }
                metadata.put(key, value);
            }
        }

        String prevHash = v.prevHash();
        if (v.previousCaseId() != null) {
            if (prevHash != null) {
                throw new IllegalArgumentException("chained vector must not specify a fixed prev_hash: " + v.id());
            }
            prevHash = priorHashes.get(v.previousCaseId());
            if (prevHash == null) {
                throw new IllegalArgumentException("previous vector must precede chained vector: " + v.id());
            }
        } else if (prevHash == null || !prevHash.matches("[0-9a-f]{64}")) {
            throw new IllegalArgumentException("fixed prev_hash must be 64 lowercase hexadecimal characters: " + v.id());
        }
        return computeResolved(v, prevHash, metadata, gsonVersion);
    }

    private static Computed computeResolved(Vector v, String prevHash, TreeMap<String, String> metadata,
                                            String gsonVersion) throws Exception {
        String metadataJson = GSON.toJson(metadata);
        JsonArray fields = new JsonArray();
        fields.add(DOMAIN);
        fields.add(prevHash);
        fields.add(v.serverId());
        fields.add(v.principalId());
        fields.add(v.deviceId());
        fields.add(v.source());
        fields.add(v.action());
        fields.add(v.target());
        fields.add(v.outcome());
        fields.add(v.operationId());
        fields.add(v.atMs());
        fields.add(metadataJson);
        String chainInputJson = GSON.toJson(fields);
        String hash = HexFormat.of().formatHex(MessageDigest.getInstance("SHA-256")
                .digest(chainInputJson.getBytes(StandardCharsets.UTF_8)));

        JsonObject json = new JsonObject();
        json.addProperty("case_id", v.id());
        if (v.previousCaseId() == null) {
            json.add("previous_case_id", JsonNull.INSTANCE);
        } else {
            json.addProperty("previous_case_id", v.previousCaseId());
        }
        json.addProperty("prev_hash", prevHash);
        json.addProperty("server_id", v.serverId());
        json.addProperty("principal_id", v.principalId());
        json.addProperty("device_id", v.deviceId());
        json.addProperty("source", v.source());
        json.addProperty("action", v.action());
        json.addProperty("target", v.target());
        json.addProperty("outcome", v.outcome());
        if (v.operationId() == null) {
            json.add("operation_id", JsonNull.INSTANCE);
        } else {
            json.addProperty("operation_id", v.operationId());
        }
        json.addProperty("at_ms", v.atMs());
        if (v.metadataInput() == null) {
            json.add("metadata_input", JsonNull.INSTANCE);
        } else {
            JsonArray inputEntries = new JsonArray();
            for (Map.Entry<String, String> entry : v.metadataInput()) {
                JsonArray pair = new JsonArray();
                pair.add(entry.getKey());
                if (entry.getValue() == null) {
                    pair.add(JsonNull.INSTANCE);
                } else {
                    pair.add(entry.getValue());
                }
                inputEntries.add(pair);
            }
            json.add("metadata_input", inputEntries);
        }
        json.addProperty("metadata_json", metadataJson);
        json.addProperty("chain_input_json", chainInputJson);
        json.addProperty("sha256", hash);
        json.addProperty("gson_version", gsonVersion);
        return new Computed(json, hash);
    }

    private static String requireText(String name, String value, int maxLength) {
        if (value == null || value.isBlank() || value.length() > maxLength) {
            throw new IllegalArgumentException(name + " violates AuditRecord/AuditActor bounds");
        }
        for (int i = 0; i < value.length(); i++) {
            if (Character.isISOControl(value.charAt(i))) {
                throw new IllegalArgumentException(name + " contains forbidden control characters");
            }
        }
        return value;
    }
}
