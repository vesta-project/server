package com.vesta.wrapper.operations;

import com.vesta.wrapper.authorization.Capability;
import com.vesta.wrapper.state.StateValues;

import java.util.List;
import java.util.Objects;
import java.util.Set;
import java.util.TreeSet;

/**
 * Trusted, immutable registration binding one handler/operation type to its exact authorization
 * requirements. Everything security-relevant is derived from the handler's validated typed input —
 * canonical stored form, target, capability requirements, lock resources — and the handler contract
 * (idempotency/confirmation flags). Request-supplied type/target/flags/locks never select or weaken it.
 *
 * <p>Registrations are trusted code: a binding that derives too-weak requirements is a code defect this
 * layer cannot detect.
 */
public final class OperationBinding<I> {
    /** Upper bound on requirements per operation, enforced before any policy evaluation loop. */
    public static final int MAX_REQUIREMENTS = 32;

    /**
     * One capability that must be allowed on one resource. For scoped (file) capabilities the resource is
     * a server-relative path ({@code ""} = root) normalized and matched by the policy; for unscoped
     * capabilities use {@code ""}. Wildcards are never accepted as a requirement resource.
     */
    public record Requirement(Capability capability, String resource) {
        public Requirement {
            Objects.requireNonNull(capability, "capability");
            Objects.requireNonNull(resource, "resource");
            if (resource.length() > 512 || resource.indexOf('*') >= 0
                    || resource.chars().anyMatch(Character::isISOControl)) {
                throw new OperationException(OperationException.INVALID_REQUEST, "malformed requirement resource");
            }
            if (!capability.scoped() && !resource.isEmpty()) {
                throw new OperationException(OperationException.INVALID_REQUEST,
                        "unscoped capability requirement must not name a resource");
            }
        }
    }

    /** Pure derivations from a parsed input. Must be deterministic and side-effect free. */
    public interface Derivation<I> {
        /** Canonical serialized form persisted and hash-bound; must re-parse to an equal canonical form. */
        String canonicalInput(I input);

        /** Canonical operation target (shown, audited, bound into confirmations). */
        String target(I input);

        /** Exact, non-empty capability requirements; there is no group/wildcard fallback. */
        List<Requirement> requirements(I input);
    }

    /** Fully derived, validated binding of one canonical input. */
    record Derived(String type, String canonicalInput, String target, List<Requirement> requirements,
                   List<String> lockResources, HandlerContract contract) {
    }

    private final OperationHandler<I> handler;
    private final Derivation<I> derivation;

    private OperationBinding(OperationHandler<I> handler, Derivation<I> derivation) {
        this.handler = handler;
        this.derivation = derivation;
    }

    public static <I> OperationBinding<I> of(OperationHandler<I> handler, Derivation<I> derivation) {
        Objects.requireNonNull(handler, "handler");
        Objects.requireNonNull(derivation, "derivation");
        StateValues.requireText("handler type", handler.type(), 64);
        Objects.requireNonNull(handler.contract(), "handler contract");
        return new OperationBinding<>(handler, derivation);
    }

    public String type() {
        return handler.type();
    }

    public OperationHandler<I> handler() {
        return handler;
    }

    /**
     * Parses and derives everything from {@code canonicalJson}. Any malformed or unstable result throws
     * {@link OperationFailure} ({@code invalid_input}, {@code invalid_target}, {@code invalid_requirements},
     * {@code invalid_resources}).
     */
    Derived derive(String canonicalJson) throws OperationFailure {
        if (canonicalJson == null || canonicalJson.length() > OperationRequest.MAX_PAYLOAD_CHARS) {
            throw new OperationFailure("invalid_input", "Operation input missing or too large");
        }
        try {
            I input = handler.parseInput(canonicalJson);
            if (input == null) {
                throw new OperationFailure("invalid_input", "Operation input could not be parsed");
            }
            String canonical = derivation.canonicalInput(input);
            if (canonical == null || canonical.length() > OperationRequest.MAX_PAYLOAD_CHARS) {
                throw new OperationFailure("invalid_input", "Canonical input missing or too large");
            }
            // All security derivations are bound to the persisted canonical form's reparse.
            I reparsed = handler.parseInput(canonical);
            if (reparsed == null || !canonical.equals(derivation.canonicalInput(reparsed))) {
                throw new OperationFailure("invalid_input", "Canonical input is not stable");
            }
            Semantics bound = semantics(reparsed);
            // The round trip must preserve meaning, not just text: the original parse must derive the same
            // target, requirements and resources as the canonical form that is persisted and rechecked.
            if (!bound.equals(semantics(input))) {
                throw new OperationFailure("invalid_input", "Canonicalization changed the operation's meaning");
            }
            return new Derived(handler.type(), canonical, bound.target(), bound.requirements(), bound.resources(),
                    handler.contract());
        } catch (OperationFailure f) {
            throw f;
        } catch (RuntimeException e) {
            throw new OperationFailure("invalid_input", "Operation input could not be validated");
        }
    }

    private record Semantics(String target, List<Requirement> requirements, List<String> resources) {
    }

    /** Validated target, bounded requirements and sorted lock resources of one parsed input. */
    private Semantics semantics(I input) throws OperationFailure {
        String target = derivation.target(input);
        try {
            StateValues.requireText("target", target, 512);
        } catch (RuntimeException e) {
            throw new OperationFailure("invalid_target", "Derived operation target is malformed");
        }
        List<Requirement> requirements = derivation.requirements(input);
        // Note: immutable List.contains(null) throws, so test elements explicitly.
        if (requirements == null || requirements.isEmpty() || requirements.size() > MAX_REQUIREMENTS
                || requirements.stream().anyMatch(Objects::isNull)) {
            throw new OperationFailure("invalid_requirements", "Operation authorization requirements are malformed");
        }
        Set<String> resources = handler.requiredResources(input);
        if (resources == null || resources.size() > OperationRequest.MAX_RESOURCES) {
            throw new OperationFailure("invalid_resources", "Operation lock resources are malformed");
        }
        TreeSet<String> sorted = new TreeSet<>();
        for (String r : resources) {
            try {
                sorted.add(StateValues.requireText("lock resource", r, 256));
            } catch (RuntimeException e) {
                throw new OperationFailure("invalid_resources", "Operation lock resources are malformed");
            }
        }
        return new Semantics(target, List.copyOf(requirements), List.copyOf(sorted));
    }
}
