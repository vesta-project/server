package com.vesta.wrapper.operations;

/** Result of {@link OperationService#submit}: {@code created=false} means an idempotent replay. */
public record SubmitResult(Operation operation, boolean created) {
}
