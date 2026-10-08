import java.lang.reflect.Method;
import java.util.ArrayList;
import java.util.Arrays;
import java.util.LinkedHashSet;
import java.util.List;
import java.util.Set;

/**
 * Emits a stable JSON snapshot of the built production identity constants.
 *
 * <p>Run with the cached Java 21 source launcher and the already-built wrapper classes on the
 * classpath. This deliberately uses reflection so the oracle reads production bytecode rather than
 * embedding expected capability or role values. Role capabilities are emitted in Capability.values()
 * order because the production preset exposes a Set.
 */
public final class IdentityConstantsOracle {
    private static final String CAPABILITY_CLASS = "com.vesta.wrapper.authorization.Capability";
    private static final String BUILTIN_ROLES_CLASS = "com.vesta.wrapper.authorization.BuiltinRoles";

    private IdentityConstantsOracle() {
    }

    public static void main(String[] args) throws Exception {
        if (args.length != 0) {
            throw new IllegalArgumentException("usage: IdentityConstantsOracle");
        }

        Class<?> capabilityType = Class.forName(CAPABILITY_CLASS);
        Object[] capabilities = (Object[]) capabilityType.getMethod("values").invoke(null);
        Method idMethod = capabilityType.getMethod("id");
        Method scopedMethod = capabilityType.getMethod("scoped");
        Method localOnlyMethod = capabilityType.getMethod("localOnly");
        Method ownerReservedMethod = capabilityType.getMethod("ownerReserved");
        Method adminReservedMethod = capabilityType.getMethod("adminReserved");
        Method delegableMethod = capabilityType.getMethod("delegable");

        Class<?> rolesType = Class.forName(BUILTIN_ROLES_CLASS);
        Object localOnlyGrantIds = rolesType.getField("LOCAL_ONLY_GRANT").get(null);
        if (!(localOnlyGrantIds instanceof Set<?> localOnlySet)) {
            throw new IllegalStateException("BuiltinRoles.LOCAL_ONLY_GRANT is not a Set");
        }
        Set<String> localGrantOnly = new LinkedHashSet<>();
        for (Object roleId : localOnlySet) {
            if (!(roleId instanceof String stringId)) {
                throw new IllegalStateException("non-string local-grant-only role id");
            }
            localGrantOnly.add(stringId);
        }

        List<?> presets = (List<?>) rolesType.getMethod("presets").invoke(null);
        List<Object> roles = new ArrayList<>(presets);
        StringBuilder json = new StringBuilder(16_384);
        json.append("{\n  \"provenance\": {\n")
                .append("    \"oracle\": \"scripts/IdentityConstantsOracle.java\",\n")
                .append("    \"productionClasses\": \"wrapper/build/classes/java/main\",\n")
                .append("    \"productionSources\": [\n")
                .append("      \"wrapper/src/main/java/com/vesta/wrapper/authorization/Capability.java\",\n")
                .append("      \"wrapper/src/main/java/com/vesta/wrapper/authorization/BuiltinRoles.java\"\n")
                .append("    ],\n")
                .append("    \"method\": \"reflection over already-built production classes\"\n")
                .append("  },\n  \"capabilities\": [\n");

        for (int i = 0; i < capabilities.length; i++) {
            Object capability = capabilities[i];
            if (i > 0) {
                json.append(",\n");
            }
            json.append("    {\"ordinal\": ").append(i)
                    .append(", \"enum\": ").append(quote(((Enum<?>) capability).name()))
                    .append(", \"id\": ").append(quote((String) idMethod.invoke(capability)))
                    .append(", \"scoped\": ").append(scopedMethod.invoke(capability))
                    .append(", \"localOnly\": ").append(localOnlyMethod.invoke(capability))
                    .append(", \"ownerReserved\": ").append(ownerReservedMethod.invoke(capability))
                    .append(", \"adminReserved\": ").append(adminReservedMethod.invoke(capability))
                    .append(", \"delegable\": ").append(delegableMethod.invoke(capability))
                    .append("}");
        }

        json.append("\n  ],\n  \"roles\": [\n");
        Method presetId = null;
        Method presetName = null;
        Method presetRank = null;
        Method presetCapabilities = null;
        if (!roles.isEmpty()) {
            Class<?> presetType = roles.get(0).getClass();
            presetId = presetType.getMethod("id");
            presetName = presetType.getMethod("name");
            presetRank = presetType.getMethod("rank");
            presetCapabilities = presetType.getMethod("capabilities");
        }
        for (int i = 0; i < roles.size(); i++) {
            Object preset = roles.get(i);
            if (i > 0) {
                json.append(",\n");
            }
            String roleId = (String) presetId.invoke(preset);
            String roleName = (String) presetName.invoke(preset);
            int rank = (Integer) presetRank.invoke(preset);
            Object rawRoleCapabilities = presetCapabilities.invoke(preset);
            if (!(rawRoleCapabilities instanceof Set<?> roleCapabilitySet)) {
                throw new IllegalStateException("preset capabilities is not a Set: " + roleId);
            }
            Set<Object> roleCapabilityLookup = new LinkedHashSet<>(roleCapabilitySet);

            json.append("    {\"id\": ").append(quote(roleId))
                    .append(", \"name\": ").append(quote(roleName))
                    .append(", \"rank\": ").append(rank)
                    .append(", \"localGrantOnly\": ").append(localGrantOnly.contains(roleId))
                    .append(", \"capabilities\": [");
            boolean first = true;
            for (Object capability : capabilities) {
                if (roleCapabilityLookup.contains(capability)) {
                    if (!first) {
                        json.append(", ");
                    }
                    json.append(quote((String) idMethod.invoke(capability)));
                    first = false;
                }
            }
            if (roleCapabilityLookup.size() != roleCapabilityLookup.stream()
                    .filter(roleCapability -> Arrays.asList(capabilities).contains(roleCapability)).count()) {
                throw new IllegalStateException("preset contains a capability outside Capability.values(): " + roleId);
            }
            json.append("]}");
        }
        json.append("\n  ]\n}\n");
        System.out.print(json);
    }

    private static String quote(String value) {
        StringBuilder escaped = new StringBuilder(value.length() + 2).append('"');
        for (int i = 0; i < value.length(); i++) {
            char c = value.charAt(i);
            switch (c) {
                case '"' -> escaped.append("\\\"");
                case '\\' -> escaped.append("\\\\");
                case '\b' -> escaped.append("\\b");
                case '\f' -> escaped.append("\\f");
                case '\n' -> escaped.append("\\n");
                case '\r' -> escaped.append("\\r");
                case '\t' -> escaped.append("\\t");
                default -> {
                    if (c < 0x20) {
                        escaped.append(String.format("\\u%04x", (int) c));
                    } else {
                        escaped.append(c);
                    }
                }
            }
        }
        return escaped.append('"').toString();
    }
}
