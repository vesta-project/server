package com.vesta.wrapper.files;

import java.io.IOException;
import java.io.InputStream;
import java.nio.file.AtomicMoveNotSupportedException;
import java.nio.file.Files;
import java.nio.file.StandardCopyOption;
import java.nio.file.LinkOption;
import java.nio.file.NoSuchFileException;
import java.nio.file.Path;
import java.nio.file.attribute.BasicFileAttributes;
import java.util.ArrayList;
import java.util.Collection;
import java.util.List;
import java.util.Locale;
import java.util.Objects;
import java.util.Set;

/**
 * Shared confinement policy for every file route that takes a client-supplied relative path
 * (file browser list/read/write/delete and content install/remove/serve).
 *
 * <p>Enforces, independent of the host OS:
 * <ul>
 *   <li>lexical rules: relative only, no {@code .}/{@code ..}/empty segments, no control
 *       characters, no Windows drive/UNC/alternate-data-stream syntax ({@code :}), no Windows-reserved
 *       characters, no trailing dot/space, no DOS device names ({@code CON}, {@code NUL}, {@code COM1}...);</li>
 *   <li>invariant protected internals ({@code .vesta}, VCS metadata, wrapper config/install,
 *       plus any configured absolute paths such as a TLS keystore) are never listed or reachable;</li>
 *   <li>every <em>existing</em> component from the root down is inspected without following links,
 *       and any symlink, junction, or other reparse point fails closed.</li>
 * </ul>
 *
 * <p>Residual risk: these are check-then-use path checks in Java. A local attacker who can
 * concurrently rename/replace an ancestor directory with a link between the check and the
 * subsequent open can still race it. Eliminating that requires OS-level isolation (dedicated
 * low-privilege user, handle-relative {@code openat}/{@code O_NOFOLLOW} or Windows
 * {@code FILE_FLAG_OPEN_REPARSE_POINT} walks), which is out of scope here.
 */
public final class ServerPathPolicy {

    /** Path segment names (case-insensitive, at any depth) that are never exposed. */
    public static final Set<String> PROTECTED_NAMES = Set.of(
            ".vesta",
            ".git",
            ".hg",
            ".svn",
            "wrapper-config.toml",
            "vesta-wrapper",
            "start-vesta-wrapper.sh",
            "start-vesta-wrapper.bat"
    );

    public static final int MAX_SEGMENT_LENGTH = 255;
    public static final int MAX_PATH_LENGTH = 4096;

    private static final String WINDOWS_RESERVED_CHARS = "<>:\"|?*";
    private static final Set<String> DEVICE_NAMES;
    static {
        List<String> names = new ArrayList<>(List.of("con", "prn", "aux", "nul", "conin$", "conout$", "clock$"));
        for (String prefix : List.of("com", "lpt")) {
            for (char c = '0'; c <= '9'; c++) {
                names.add(prefix + c);
            }
            names.add(prefix + '¹');
            names.add(prefix + '²');
            names.add(prefix + '³');
        }
        DEVICE_NAMES = Set.copyOf(names);
    }

    private static final int FILE_ATTRIBUTE_REPARSE_POINT = 0x400;

    private final Path root;
    private final List<Path> extraProtected;

    public ServerPathPolicy(Path root) {
        this(root, List.of());
    }

    /**
     * @param root           confinement root (need not exist yet)
     * @param extraProtected additional absolute or root-relative paths to deny (e.g. TLS keystore,
     *                       private state roots). Paths outside the root are ignored, since they are
     *                       already unreachable.
     */
    public ServerPathPolicy(Path root, Collection<Path> extraProtected) {
        this.root = Objects.requireNonNull(root, "root").toAbsolutePath().normalize();
        List<Path> extras = new ArrayList<>();
        if (extraProtected != null) {
            for (Path p : extraProtected) {
                if (p != null) {
                    extras.add((p.isAbsolute() ? p : this.root.resolve(p)).toAbsolutePath().normalize());
                }
            }
        }
        this.extraProtected = List.copyOf(extras);
    }

    public Path root() {
        return root;
    }

    /**
     * Validates and canonicalises a client relative path to {@code a/b/c} form. A single trailing
     * slash is tolerated. Throws {@link SecurityException} for any rejected form.
     *
     * @param allowEmpty when true, null/blank means the root and returns {@code ""}
     */
    public static String normalize(String relativePath, boolean allowEmpty) {
        if (relativePath == null || relativePath.isEmpty()) {
            if (allowEmpty) {
                return "";
            }
            throw new SecurityException("path must not be empty");
        }
        if (relativePath.length() > MAX_PATH_LENGTH) {
            throw new SecurityException("path too long");
        }
        String p = relativePath.replace('\\', '/');
        if (p.startsWith("/")) {
            throw new SecurityException("absolute paths are not allowed");
        }
        while (p.endsWith("/")) {
            p = p.substring(0, p.length() - 1);
        }
        if (p.isEmpty()) {
            if (allowEmpty) {
                return "";
            }
            throw new SecurityException("path must not be empty");
        }
        for (String segment : p.split("/", -1)) {
            validateSegment(segment);
        }
        return p;
    }

    private static void validateSegment(String segment) {
        if (segment.isEmpty()) {
            throw new SecurityException("empty path segment");
        }
        if (segment.equals(".") || segment.equals("..")) {
            throw new SecurityException("relative path segments are not allowed");
        }
        if (segment.length() > MAX_SEGMENT_LENGTH) {
            throw new SecurityException("path segment too long");
        }
        for (int i = 0; i < segment.length(); i++) {
            char c = segment.charAt(i);
            if (Character.isISOControl(c) || WINDOWS_RESERVED_CHARS.indexOf(c) >= 0) {
                throw new SecurityException("invalid character in path");
            }
        }
        char last = segment.charAt(segment.length() - 1);
        if (last == '.' || last == ' ') {
            throw new SecurityException("path segment must not end with a dot or space");
        }
        String base = segment;
        int dot = base.indexOf('.');
        if (dot >= 0) {
            base = base.substring(0, dot);
        }
        base = base.stripTrailing().toLowerCase(Locale.ROOT);
        if (DEVICE_NAMES.contains(base)) {
            throw new SecurityException("reserved device name in path");
        }
    }

    /** True when a single (already on-disk or requested) segment name is an invariant protected name. */
    public static boolean isProtectedName(String name) {
        return name != null && PROTECTED_NAMES.contains(name.toLowerCase(Locale.ROOT));
    }

    /**
     * Resolves a client path to a confined absolute path. Every existing component is verified to be
     * a plain file/directory (no link or reparse point) whose real name is not protected; any
     * non-existent tail is returned as-is beneath the last verified directory.
     */
    public Path resolve(String relativePath, boolean allowRoot) throws IOException {
        String normalized = normalize(relativePath, allowRoot);
        Path base = realRoot();
        if (normalized.isEmpty()) {
            return base;
        }
        String[] segments = normalized.split("/");
        Path current = base;
        boolean existing = true;
        for (String segment : segments) {
            if (isProtectedName(segment)) {
                throw new SecurityException("path is protected");
            }
            Path child = current.resolve(segment);
            if (existing) {
                BasicFileAttributes attrs = readNoFollow(child);
                if (attrs == null) {
                    existing = false;
                } else {
                    assertPlain(child, attrs);
                    // Canonical on-disk name: defeats case and 8.3 short-name aliases of protected names.
                    Path real = child.toRealPath(LinkOption.NOFOLLOW_LINKS);
                    Path realName = real.getFileName();
                    if (realName == null || isProtectedName(realName.toString())) {
                        throw new SecurityException("path is protected");
                    }
                    if (!real.getParent().equals(current) && !sameIgnoringCase(real.getParent(), current)) {
                        throw new SecurityException("path escapes server root");
                    }
                    child = current.resolve(realName.toString());
                }
            }
            current = child;
        }
        if (!current.startsWith(base)) {
            throw new SecurityException("path escapes server root");
        }
        if (isExtraProtected(base, current)) {
            throw new SecurityException("path is protected");
        }
        return current;
    }

    /**
     * Creates any missing directories between the root and {@code dir} (which must already have been
     * returned by {@link #resolve}), re-verifying each component after creation.
     */
    public void createDirectories(Path dir) throws IOException {
        Path base = realRoot();
        if (!dir.startsWith(base)) {
            throw new SecurityException("path escapes server root");
        }
        Path current = base;
        for (Path segment : base.relativize(dir)) {
            current = current.resolve(segment.toString());
            BasicFileAttributes attrs = readNoFollow(current);
            if (attrs == null) {
                try {
                    Files.createDirectory(current);
                } catch (java.nio.file.FileAlreadyExistsException raced) {
                    // fall through to re-verification
                }
                attrs = readNoFollow(current);
                if (attrs == null) {
                    throw new IOException("could not create directory");
                }
            }
            assertPlain(current, attrs);
            if (!attrs.isDirectory()) {
                throw new IOException("not a directory: " + base.relativize(current).toString().replace('\\', '/'));
            }
        }
    }

    /**
     * Writes {@code data} to a temp file beside {@code target} (already confined via {@link #resolve} and
     * whose parent exists), then atomically renames it over the target. Refuses to replace anything but
     * a regular file. No size cap is applied here; callers bound input when a quota is configured.
     */
    public static void atomicReplace(Path target, InputStream data) throws IOException {
        Path parent = target.getParent();
        assertReplaceable(target);
        Path tmp = Files.createTempFile(parent, ".vesta-", ".part");
        try {
            Files.copy(data, tmp, StandardCopyOption.REPLACE_EXISTING);
            assertReplaceable(target);
            try {
                Files.move(tmp, target, StandardCopyOption.REPLACE_EXISTING, StandardCopyOption.ATOMIC_MOVE);
            } catch (AtomicMoveNotSupportedException e) {
                Files.move(tmp, target, StandardCopyOption.REPLACE_EXISTING);
            }
        } catch (IOException | RuntimeException error) {
            Files.deleteIfExists(tmp);
            throw error;
        }
    }

    private static void assertReplaceable(Path target) throws IOException {
        BasicFileAttributes existing = readNoFollow(target);
        if (existing != null) {
            assertPlain(target, existing);
            if (!existing.isRegularFile()) {
                throw new IOException("target is not a regular file");
            }
        }
    }

    /** True when a directory entry should be hidden from listings (protected name or configured path). */
    public boolean isHidden(Path entry) {
        Path name = entry.getFileName();
        if (name == null || isProtectedName(name.toString())) {
            return true;
        }
        try {
            return isExtraProtected(realRoot(), entry.toAbsolutePath().normalize());
        } catch (IOException e) {
            return true;
        }
    }

    /** Returns attributes without following links, or {@code null} when the path does not exist. */
    public static BasicFileAttributes readNoFollow(Path path) throws IOException {
        try {
            return Files.readAttributes(path, BasicFileAttributes.class, LinkOption.NOFOLLOW_LINKS);
        } catch (NoSuchFileException e) {
            return null;
        }
    }

    /** Rejects symlinks, junctions, and any other reparse point or special file. */
    public static void assertPlain(Path path, BasicFileAttributes attrs) {
        if (attrs.isSymbolicLink() || attrs.isOther() || (!attrs.isDirectory() && !attrs.isRegularFile())) {
            throw new SecurityException("links and special files are not allowed");
        }
        if (hasReparseAttribute(path)) {
            throw new SecurityException("links and special files are not allowed");
        }
    }

    private static boolean hasReparseAttribute(Path path) {
        try {
            Object raw = Files.getAttribute(path, "dos:attributes", LinkOption.NOFOLLOW_LINKS);
            return raw instanceof Integer bits && (bits & FILE_ATTRIBUTE_REPARSE_POINT) != 0;
        } catch (UnsupportedOperationException | IllegalArgumentException | IOException e) {
            return false; // non-Windows: isSymbolicLink/isOther already cover links
        }
    }

    private Path realRoot() throws IOException {
        if (!Files.exists(root)) {
            throw new NoSuchFileException("server root does not exist");
        }
        return root.toRealPath();
    }

    private boolean isExtraProtected(Path realBase, Path target) {
        for (Path protectedPath : extraProtected) {
            for (Path candidate : protectedForms(protectedPath)) {
                Path rel = null;
                if (candidate.startsWith(root)) {
                    rel = root.relativize(candidate);
                } else if (candidate.startsWith(realBase)) {
                    rel = realBase.relativize(candidate);
                }
                if (rel == null || rel.toString().isEmpty()) {
                    continue;
                }
                Path relTarget = realBase.relativize(target);
                if (startsWithIgnoringCase(relTarget, rel)) {
                    return true;
                }
            }
        }
        return false;
    }

    private static List<Path> protectedForms(Path p) {
        List<Path> forms = new ArrayList<>(2);
        forms.add(p);
        try {
            if (Files.exists(p)) {
                forms.add(p.toRealPath());
            }
        } catch (IOException ignored) {
            // lexical form still applies
        }
        return forms;
    }

    private static boolean startsWithIgnoringCase(Path target, Path prefix) {
        if (target.getNameCount() < prefix.getNameCount()) {
            return false;
        }
        for (int i = 0; i < prefix.getNameCount(); i++) {
            if (!target.getName(i).toString().equalsIgnoreCase(prefix.getName(i).toString())) {
                return false;
            }
        }
        return true;
    }

    private static boolean sameIgnoringCase(Path a, Path b) {
        return a.toString().equalsIgnoreCase(b.toString());
    }
}
