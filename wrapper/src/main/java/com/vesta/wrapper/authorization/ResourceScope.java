package com.vesta.wrapper.authorization;

import com.vesta.wrapper.files.ServerPathPolicy;
import com.vesta.wrapper.state.StateException;

import java.util.Objects;

/**
 * A normalized resource scope for a capability: {@code *} (everything), an exact server-relative path
 * {@code a/b}, or a subtree {@code a/b/**} (the directory itself and everything beneath it).
 *
 * <p>Allows match case-sensitively; denies match case-insensitively, so a case-variant path on a
 * case-insensitive filesystem can never slip past a deny (fail closed).
 */
public final class ResourceScope {
    public static final ResourceScope ALL = new ResourceScope(Kind.ALL, "");

    private enum Kind { ALL, EXACT, SUBTREE }

    private final Kind kind;
    private final String path;

    private ResourceScope(Kind kind, String path) {
        this.kind = kind;
        this.path = path;
    }

    /**
     * Parses a persisted or requested scope through the shared {@link ServerPathPolicy#normalize} rules.
     * Accepted path aliases are canonicalized (e.g. {@code logs\x} and a trailing slash become
     * {@code logs/x}); invalid forms (absolute, drive/UNC, {@code .}/{@code ..}, control characters,
     * reserved names, wildcards other than a trailing {@code /**}) are rejected. Use {@link #text()} for
     * the canonical persisted form.
     */
    public static ResourceScope parse(String text) {
        if (text == null || text.isEmpty()) {
            throw new StateException("invalid_request", "scope is required");
        }
        if (text.equals("*") || text.equals("**")) {
            return ALL;
        }
        try {
            if (text.endsWith("/**")) {
                String prefix = text.substring(0, text.length() - 3);
                return new ResourceScope(Kind.SUBTREE, canonical(prefix));
            }
            return new ResourceScope(Kind.EXACT, canonical(text));
        } catch (SecurityException e) {
            throw new StateException("invalid_request", "invalid scope: " + e.getMessage());
        }
    }

    private static String canonical(String p) {
        String normalized = ServerPathPolicy.normalize(p, false);
        if (normalized.contains("*")) {
            throw new SecurityException("wildcards are only allowed as a trailing /**");
        }
        return normalized;
    }

    public boolean isAll() {
        return kind == Kind.ALL;
    }

    /** Canonical persisted form. */
    public String text() {
        return switch (kind) {
            case ALL -> "*";
            case EXACT -> path;
            case SUBTREE -> path + "/**";
        };
    }

    /** True when this allow-scope grants {@code resource} (a normalized relative path, "" = root). */
    public boolean allows(String resource) {
        return matches(resource, false);
    }

    /** True when this deny-scope applies to {@code resource}; case-insensitive. */
    public boolean denies(String resource) {
        return matches(resource, true);
    }

    private boolean matches(String resource, boolean ignoreCase) {
        if (kind == Kind.ALL) {
            return true;
        }
        if (resource == null) {
            return false;
        }
        if (kind == Kind.EXACT) {
            return eq(resource, path, ignoreCase);
        }
        return eq(resource, path, ignoreCase)
                || (resource.length() > path.length()
                && resource.charAt(path.length()) == '/'
                && eq(resource.substring(0, path.length()), path, ignoreCase));
    }

    /** True when every resource in {@code other} is also allowed by this scope. */
    public boolean covers(ResourceScope other) {
        if (kind == Kind.ALL) {
            return true;
        }
        if (other.kind == Kind.ALL) {
            return false;
        }
        if (kind == Kind.EXACT) {
            return other.kind == Kind.EXACT && path.equals(other.path);
        }
        return allows(other.path);
    }

    /** True when the two scopes could share any resource (case-insensitively, for deny checks). */
    public boolean overlaps(ResourceScope other) {
        return this.denies(other.kind == Kind.ALL ? null : other.path) || other.denies(this.kind == Kind.ALL ? null : path)
                || kind == Kind.ALL || other.kind == Kind.ALL;
    }

    private static boolean eq(String a, String b, boolean ignoreCase) {
        return ignoreCase ? a.equalsIgnoreCase(b) : a.equals(b);
    }

    @Override
    public boolean equals(Object o) {
        return o instanceof ResourceScope s && s.kind == kind && s.path.equals(path);
    }

    @Override
    public int hashCode() {
        return Objects.hash(kind, path);
    }

    @Override
    public String toString() {
        return text();
    }
}
