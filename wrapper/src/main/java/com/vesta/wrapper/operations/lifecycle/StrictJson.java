package com.vesta.wrapper.operations.lifecycle;

import com.google.gson.Strictness;
import com.google.gson.stream.JsonReader;
import com.google.gson.stream.JsonToken;
import com.vesta.wrapper.operations.OperationFailure;

import java.io.IOException;
import java.io.StringReader;
import java.util.LinkedHashMap;
import java.util.Map;
import java.util.Set;

/**
 * Minimal strict parser for flat lifecycle inputs: exactly one JSON object whose members are all in
 * {@code allowed}, each a JSON string, no duplicates, no trailing content. Anything else is
 * {@code invalid_input}. Side-effect free.
 */
final class StrictJson {
    static final int MAX_CHARS = 1024;
    private static final int MAX_STRING_CHARS = 128;

    private StrictJson() {
    }

    static Map<String, String> parseObject(String json, Set<String> allowed) throws OperationFailure {
        if (json == null || json.length() > MAX_CHARS) {
            throw invalid("Input missing or too large");
        }
        Map<String, String> out = new LinkedHashMap<>();
        try (JsonReader reader = new JsonReader(new StringReader(json))) {
            reader.setStrictness(Strictness.STRICT);
            if (reader.peek() != JsonToken.BEGIN_OBJECT) {
                throw invalid("Input must be a JSON object");
            }
            reader.beginObject();
            while (reader.hasNext()) {
                String name = reader.nextName();
                if (!allowed.contains(name)) {
                    throw invalid("Unknown input field");
                }
                if (out.containsKey(name)) {
                    throw invalid("Duplicate input field");
                }
                if (reader.peek() != JsonToken.STRING) {
                    throw invalid("Input field must be a string");
                }
                String value = reader.nextString();
                if (value.length() > MAX_STRING_CHARS) {
                    throw invalid("Input field too long");
                }
                out.put(name, value);
            }
            reader.endObject();
            if (reader.peek() != JsonToken.END_DOCUMENT) {
                throw invalid("Trailing content after input object");
            }
        } catch (OperationFailure f) {
            throw f;
        } catch (IOException | RuntimeException e) {
            throw invalid("Input is not valid JSON");
        }
        return out;
    }

    private static OperationFailure invalid(String message) {
        return new OperationFailure("invalid_input", message);
    }
}
