package com.vesta.wrapper.operations;

import java.util.ArrayList;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import java.util.Objects;
import java.util.Optional;

/**
 * Immutable trusted registry of {@link OperationBinding}s. It is the single source for both the
 * submission boundary/authority adapter and the runner's {@link HandlerRegistry}, so a type can only
 * execute through the handler it is authorized against.
 */
public final class OperationBindings {
    private final Map<String, OperationBinding<?>> bindings;
    private final HandlerRegistry handlers;

    private OperationBindings(Map<String, OperationBinding<?>> bindings) {
        this.bindings = Map.copyOf(bindings);
        List<OperationHandler<?>> list = new ArrayList<>();
        for (OperationBinding<?> b : bindings.values()) {
            list.add(b.handler());
        }
        this.handlers = HandlerRegistry.of(list);
    }

    public static OperationBindings of(List<? extends OperationBinding<?>> bindings) {
        Map<String, OperationBinding<?>> map = new LinkedHashMap<>();
        for (OperationBinding<?> b : bindings) {
            Objects.requireNonNull(b, "binding");
            if (map.putIfAbsent(b.type(), b) != null) {
                throw new IllegalArgumentException("Duplicate binding for operation type " + b.type());
            }
        }
        return new OperationBindings(map);
    }

    public Optional<OperationBinding<?>> find(String type) {
        return type == null ? Optional.empty() : Optional.ofNullable(bindings.get(type));
    }

    /** Handler registry for {@link OperationRunner}, containing exactly the bound handlers. */
    public HandlerRegistry handlers() {
        return handlers;
    }
}
