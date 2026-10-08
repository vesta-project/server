import java.nio.charset.StandardCharsets;
import java.time.Instant;
import java.util.ArrayList;
import java.util.List;
import java.util.Locale;

/** Standalone Java oracle for text compatibility vectors. */
public final class JavaTextOracle {
    private record Comparison(String id, String left, String right) { }
    private record RootLowerComparison(String id, String kind, String left, String right) { }
    private record InstantVector(String id, long epochMilli) { }

    private JavaTextOracle() { }

    public static void main(String[] args) throws Exception {
        if (args.length != 0) {
            throw new IllegalArgumentException("JavaTextOracle takes no arguments");
        }
        if (Runtime.version().feature() != 21) {
            throw new IllegalStateException("JavaTextOracle requires the wrapper Java 21 runtime");
        }
        System.out.write(document().getBytes(StandardCharsets.UTF_8));
        System.out.write('\n');
        System.out.flush();
    }

    private static String document() {
        StringBuilder out = new StringBuilder(128_000);
        out.append("{\n");
        out.append("  \"formatVersion\": 1,\n");
        out.append("  \"referenceJavaVersion\": ").append(quote(Runtime.version().toString())).append(",\n");
        out.append("  \"referenceJavaVendor\": ").append(quote(System.getProperty("java.vendor"))).append(",\n");
        out.append("  \"referenceJavaVm\": ").append(quote(System.getProperty("java.vm.name"))).append(",\n");
        out.append("  \"simpleCaseAlgorithm\": \"java.lang.Character.toUpperCase(int)/toLowerCase(int)\",\n");
        out.append("  \"simpleCaseUpper\": [\n");
        appendCaseMappings(out, true);
        out.append("\n  ],\n");
        out.append("  \"simpleCaseLower\": [\n");
        appendCaseMappings(out, false);
        out.append("\n  ],\n");
        out.append("  \"caseComparisons\": [\n");
        appendComparisons(out);
        out.append("\n  ],\n");
        out.append("  \"localeRootLowerComparisons\": [\n");
        appendRootLowerComparisons(out);
        out.append("\n  ],\n");
        out.append("  \"instants\": [\n");
        appendInstants(out);
        out.append("\n  ]\n");
        out.append('}');
        return out.toString();
    }

    private static void appendCaseMappings(StringBuilder out, boolean upper) {
        boolean first = true;
        for (int codePoint = 0; codePoint <= Character.MAX_CODE_POINT; codePoint++) {
            if (codePoint >= Character.MIN_SURROGATE && codePoint <= Character.MAX_SURROGATE) {
                continue;
            }
            int mapped = upper
                    ? Character.toUpperCase(codePoint)
                    : Character.toLowerCase(codePoint);
            if (mapped == codePoint) {
                continue;
            }
            if (!first) {
                out.append(',');
            }
            out.append("\n    [").append(codePoint).append(", ").append(mapped).append(']');
            first = false;
        }
    }

    private static void appendComparisons(StringBuilder out) {
        List<Comparison> cases = List.of(
                new Comparison("sharp-s-vs-ss-expansion", "ß", "SS"),
                new Comparison("sharp-s-vs-capital-sharp-s", "ß", "ẞ"),
                new Comparison("turkish-lower-i-vs-ascii-i", "i", "I"),
                new Comparison("turkish-capital-dotted-i-vs-i", "İ", "i"),
                new Comparison("turkish-dotless-i-vs-ascii-i", "ı", "I"),
                new Comparison("kelvin-sign-vs-k", "K", "k"),
                new Comparison("long-s-vs-s", "ſ", "S"),
                new Comparison("greek-sigma-vs-small-sigma", "Σ", "σ"),
                new Comparison("greek-sigma-vs-final-sigma", "Σ", "ς"),
                new Comparison("greek-small-vs-final-sigma", "σ", "ς"),
                new Comparison("deseret-supplementary-pair", scalar(0x10400), scalar(0x10428)),
                new Comparison("greek-dialytika-tonos-expansion", "ΐ", "Ϊ́"),
                new Comparison("latin-ligature-ffi-expansion", "ﬃ", "FFI"),
                new Comparison("composed-vs-decomposed-acute", "é", "é")
        );
        for (int index = 0; index < cases.size(); index++) {
            Comparison vector = cases.get(index);
            if (index > 0) {
                out.append(',');
            }
            int regionLength = Math.min(vector.left().length(), vector.right().length());
            boolean equalsIgnoreCase = vector.left().equalsIgnoreCase(vector.right());
            boolean regionMatchesIgnoreCase = vector.left().regionMatches(
                    true, 0, vector.right(), 0, regionLength);
            out.append("\n    {\n");
            out.append("      \"id\": ").append(quote(vector.id())).append(",\n");
            out.append("      \"left\": ").append(quote(vector.left())).append(",\n");
            out.append("      \"right\": ").append(quote(vector.right())).append(",\n");
            out.append("      \"equalsIgnoreCase\": ").append(equalsIgnoreCase).append(",\n");
            out.append("      \"regionLengthUtf16\": ").append(regionLength).append(",\n");
            out.append("      \"regionMatchesIgnoreCase\": ").append(regionMatchesIgnoreCase).append("\n");
            out.append("    }");
        }
    }

    private static void appendRootLowerComparisons(StringBuilder out) {
        List<RootLowerComparison> cases = List.of(
                new RootLowerComparison("kelvin-sign-prefix-matches-ascii-k", "prefix", "Konfig", "kon"),
                new RootLowerComparison("dotted-i-prefix-does-not-match-ascii-i", "prefix", "İnternal", "internal"),
                new RootLowerComparison("kelvin-sign-basename-matches-clock-dollar", "basename", "clocK$", "clock$"),
                new RootLowerComparison("ascii-clock-dollar-basename-case-insensitive", "basename", "CLOCK$", "clock$"),
                new RootLowerComparison("dotted-i-basename-does-not-match-ascii-conin-dollar", "basename", "CONİN$", "CONIN$")
        );
        for (int index = 0; index < cases.size(); index++) {
            RootLowerComparison vector = cases.get(index);
            if (index > 0) {
                out.append(',');
            }
            String leftLower = vector.left().toLowerCase(Locale.ROOT);
            String rightLower = vector.right().toLowerCase(Locale.ROOT);
            boolean matches = vector.kind().equals("prefix")
                    ? leftLower.startsWith(rightLower)
                    : leftLower.equals(rightLower);
            out.append("\n    {\n");
            out.append("      \"id\": ").append(quote(vector.id())).append(",\n");
            out.append("      \"kind\": ").append(quote(vector.kind())).append(",\n");
            out.append("      \"left\": ").append(quote(vector.left())).append(",\n");
            out.append("      \"right\": ").append(quote(vector.right())).append(",\n");
            out.append("      \"leftLowerRoot\": ").append(quote(leftLower)).append(",\n");
            out.append("      \"rightLowerRoot\": ").append(quote(rightLower)).append(",\n");
            out.append("      \"matchesAfterLowerRoot\": ").append(matches).append("\n");
            out.append("    }");
        }
    }

    private static void appendInstants(StringBuilder out) {
        List<InstantVector> vectors = new ArrayList<>();
        vectors.add(new InstantVector("one-day-before-epoch-minus-one-millisecond", -86_400_001L));
        vectors.add(new InstantVector("negative-one-second-plus-minus-one-millisecond", -1_001L));
        vectors.add(new InstantVector("negative-one-second", -1_000L));
        vectors.add(new InstantVector("negative-one-second-plus-one-millisecond", -999L));
        vectors.add(new InstantVector("negative-one-millisecond", -1L));
        vectors.add(new InstantVector("epoch-zero", 0L));
        vectors.add(new InstantVector("positive-one-millisecond", 1L));
        vectors.add(new InstantVector("positive-fractional-millisecond-999", 999L));
        vectors.add(new InstantVector("positive-one-second", 1_000L));
        vectors.add(new InstantVector("positive-one-second-plus-one-millisecond", 1_001L));
        vectors.add(new InstantVector("negative-fractional-millisecond-123", -123L));
        vectors.add(new InstantVector("positive-fractional-millisecond-123", 123L));
        vectors.add(new InstantVector("leap-day-2000", epochMilli("2000-02-29T23:59:59.999Z")));
        vectors.add(new InstantVector("leap-day-2024", epochMilli("2024-02-29T12:34:56.789Z")));
        vectors.add(new InstantVector("non-leap-century-1900-next-day", epochMilli("1900-03-01T00:00:00Z")));
        vectors.add(new InstantVector("non-leap-century-2100-next-day", epochMilli("2100-03-01T00:00:00Z")));
        vectors.add(new InstantVector("minimum-signed-epoch-millisecond", Long.MIN_VALUE));
        vectors.add(new InstantVector("maximum-signed-epoch-millisecond", Long.MAX_VALUE));

        for (int index = 0; index < vectors.size(); index++) {
            InstantVector vector = vectors.get(index);
            if (index > 0) {
                out.append(',');
            }
            String iso = Instant.ofEpochMilli(vector.epochMilli()).toString();
            out.append("\n    {\n");
            out.append("      \"id\": ").append(quote(vector.id())).append(",\n");
            out.append("      \"epochMilli\": ").append(quote(Long.toString(vector.epochMilli()))).append(",\n");
            out.append("      \"iso\": ").append(quote(iso)).append("\n");
            out.append("    }");
        }
    }

    private static long epochMilli(String iso) {
        return Instant.parse(iso).toEpochMilli();
    }

    private static String scalar(int codePoint) {
        return new String(Character.toChars(codePoint));
    }

    private static String quote(String value) {
        StringBuilder out = new StringBuilder(value.length() + 2);
        out.append('"');
        for (int index = 0; index < value.length(); index++) {
            char character = value.charAt(index);
            switch (character) {
                case '"' -> out.append("\\\"");
                case '\\' -> out.append("\\\\");
                case '\b' -> out.append("\\b");
                case '\f' -> out.append("\\f");
                case '\n' -> out.append("\\n");
                case '\r' -> out.append("\\r");
                case '\t' -> out.append("\\t");
                default -> {
                    if (character < 0x20) {
                        out.append("\\u");
                        appendHex4(out, character);
                    } else {
                        out.append(character);
                    }
                }
            }
        }
        out.append('"');
        return out.toString();
    }

    private static void appendHex4(StringBuilder out, char value) {
        final char[] hex = "0123456789abcdef".toCharArray();
        out.append(hex[(value >>> 12) & 0xf]);
        out.append(hex[(value >>> 8) & 0xf]);
        out.append(hex[(value >>> 4) & 0xf]);
        out.append(hex[value & 0xf]);
    }
}
