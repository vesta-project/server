package com.vesta.wrapper.operations;

import com.vesta.wrapper.state.StateValues;

import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import java.util.Objects;
import java.util.Optional;

/** Immutable set of registered handlers keyed by operation type. */
public final class HandlerRegistry {
    private final Map<String, OperationHandler<?>> handlers;

    private HandlerRegistry(Map<String, OperationHandler<?>> handlers) {
        this.handlers = Map.copyOf(handlers);
    }

    public static HandlerRegistry of(List<? extends OperationHandler<?>> handlers) {
        Map<String, OperationHandler<?>> map = new LinkedHashMap<>();
        for (OperationHandler<?> h : handlers) {
            Objects.requireNonNull(h, "handler");
            String type = StateValues.requireText("handler type", h.type(), 64);
            Objects.requireNonNull(h.contract(), "handler contract");
            if (map.putIfAbsent(type, h) != null) {
                throw new IllegalArgumentException("Duplicate handler for operation type " + type);
            }
        }
        return new HandlerRegistry(map);
    }

    public Optional<OperationHandler<?>> find(String type) {
        return Optional.ofNullable(handlers.get(type));
    }

    public int size() {
        return handlers.size();
    }
}
