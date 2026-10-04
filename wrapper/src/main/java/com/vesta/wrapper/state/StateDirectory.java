package com.vesta.wrapper.state;

import java.io.IOException;
import java.nio.channels.FileChannel;
import java.nio.channels.FileLock;
import java.nio.channels.OverlappingFileLockException;
import java.nio.file.StandardOpenOption;
import java.nio.file.Files;
import java.nio.file.LinkOption;
import java.nio.file.Path;
import java.nio.file.attribute.AclEntry;
import java.nio.file.attribute.AclEntryFlag;
import java.nio.file.attribute.AclEntryPermission;
import java.nio.file.attribute.AclEntryType;
import java.nio.file.attribute.AclFileAttributeView;
import java.nio.file.attribute.BasicFileAttributes;
import java.nio.file.attribute.FileOwnerAttributeView;
import java.nio.file.attribute.PosixFileAttributeView;
import java.nio.file.attribute.PosixFilePermission;
import java.nio.file.attribute.PosixFilePermissions;
import java.nio.file.attribute.UserPrincipal;
import java.util.EnumSet;
import java.util.List;
import java.util.Objects;
import java.util.Set;

/**
 * Protected local directory holding the agent's durable SQLite state.
 *
 * <p>The directory must live on local disk outside the Minecraft/file-browser root.
 * Permission tightening (POSIX owner-only modes or a Windows owner-only ACL) is local
 * hygiene for the service account; it is NOT proof that the game process cannot read
 * the state, because the game may run as the same OS account.
 */
public final class StateDirectory {
    public static final String DATABASE_FILE = "vesta-state.db";
    public static final String BACKUPS_DIR = "migration-backups";

    private static final Set<PosixFilePermission> DIR_PERMS = PosixFilePermissions.fromString("rwx------");
    private static final Set<PosixFilePermission> FILE_PERMS = PosixFilePermissions.fromString("rw-------");

    private final Path path;

    /** Accepts a state directory without a known game root (e.g. tooling). */
    public StateDirectory(Path stateDir) {
        this.path = normalize(Objects.requireNonNull(stateDir, "stateDir"));
    }

    /** Accepts a state directory and rejects it if it overlaps the game root in either direction. */
    public StateDirectory(Path stateDir, Path gameRoot) {
        this(stateDir);
        Path game = normalize(Objects.requireNonNull(gameRoot, "gameRoot"));
        if (path.startsWith(game) || game.startsWith(path)) {
            throw new StateException("state_dir_overlaps_game_root",
                    "State directory must be outside the game root (and must not contain it)");
        }
    }

    public Path path() {
        return path;
    }

    public Path databaseFile() {
        return path.resolve(DATABASE_FILE);
    }

    public Path backupsDir() {
        return path.resolve(BACKUPS_DIR);
    }

    /**
     * Creates the directory tree and tightens permissions. Fails closed if tightening is unsupported
     * or if the root, backups directory or database companion files are links/reparse points/special files.
     */
    public void prepare() {
        try {
            Files.createDirectories(path);
            requireRealDirectory(path);
            tighten(path, true);
            Path backups = backupsDir();
            if (!Files.exists(backups, LinkOption.NOFOLLOW_LINKS)) {
                Files.createDirectory(backups);
            }
            requireRealDirectory(backups);
            tighten(backups, true);
            verifyDatabaseFiles();
        } catch (IOException e) {
            throw new StateException("state_dir_unavailable", "Cannot prepare state directory", e);
        }
    }

    /**
     * Rejects a linked, reparse-point or special-file database, WAL, SHM, rollback journal or
     * backups directory. Called before every connection.
     *
     * <p>Residual TOCTOU: a process running as the same account could swap a file for a link between
     * this check and SQLite opening it. The root is trusted as a resolved path and permissions limit
     * who can do this; the check does not defend against an attacker already running as the service user.
     */
    public void verifyDatabaseFiles() {
        Path db = databaseFile();
        for (Path p : List.of(db, sibling(db, "-wal"), sibling(db, "-shm"), sibling(db, "-journal"))) {
            requireRegularOrAbsent(p);
        }
        requireRealDirectory(backupsDir());
    }

    /** Requires that {@code file} is either absent or a plain regular file (not a link/reparse/special file). */
    public static void requireRegularOrAbsent(Path file) {
        if (!Files.exists(file, LinkOption.NOFOLLOW_LINKS) && !Files.isSymbolicLink(file)) {
            return;
        }
        BasicFileAttributes attrs = attributes(file);
        if (attrs.isSymbolicLink() || attrs.isOther() || !attrs.isRegularFile()) {
            throw new StateException("state_file_linked", "State file is a link or special file: " + file.getFileName());
        }
    }

    private static void requireRealDirectory(Path dir) {
        BasicFileAttributes attrs = attributes(dir);
        if (attrs.isSymbolicLink() || attrs.isOther() || !attrs.isDirectory()) {
            throw new StateException("state_file_linked", "State directory is a link or special file: " + dir.getFileName());
        }
    }

    private static BasicFileAttributes attributes(Path p) {
        try {
            return Files.readAttributes(p, BasicFileAttributes.class, LinkOption.NOFOLLOW_LINKS);
        } catch (IOException e) {
            throw new StateException("state_file_linked", "Cannot inspect state path: " + p.getFileName(), e);
        }
    }

    private static Path sibling(Path file, String suffix) {
        return file.resolveSibling(file.getFileName() + suffix);
    }

    public static final String RUNNER_LOCK_FILE = "operation-runner.lock";

    /**
     * Acquires an exclusive OS file lock so only one operation runner (in any process) drives this
     * state database. Fails closed with {@code runner_lock_held} if another holder exists.
     */
    public AutoCloseable acquireRunnerLock() {
        Path lockFile = path.resolve(RUNNER_LOCK_FILE);
        requireRegularOrAbsent(lockFile);
        FileChannel channel = null;
        try {
            channel = FileChannel.open(lockFile, StandardOpenOption.CREATE, StandardOpenOption.WRITE,
                    LinkOption.NOFOLLOW_LINKS);
            requireRegularOrAbsent(lockFile);
            tightenFile(lockFile);
            FileLock lock = channel.tryLock();
            if (lock == null) {
                throw new StateException("runner_lock_held", "Another operation runner holds the state lock");
            }
            FileChannel held = channel;
            return () -> {
                try (held) {
                    lock.release();
                }
            };
        } catch (OverlappingFileLockException e) {
            closeQuietly(channel);
            throw new StateException("runner_lock_held", "Another operation runner holds the state lock");
        } catch (IOException e) {
            closeQuietly(channel);
            throw new StateException("runner_lock_failed", "Cannot acquire operation runner lock", e);
        } catch (RuntimeException e) {
            closeQuietly(channel);
            throw e;
        }
    }

    private static void closeQuietly(FileChannel channel) {
        if (channel != null) {
            try {
                channel.close();
            } catch (IOException ignored) {
                // best effort
            }
        }
    }

    /** Tightens a regular file created inside the state directory (database, WAL, backups). Never follows links. */
    public void tightenFile(Path file) {
        if (!Files.exists(file, LinkOption.NOFOLLOW_LINKS) && !Files.isSymbolicLink(file)) {
            return;
        }
        requireRegularOrAbsent(file);
        try {
            tighten(file, false);
        } catch (IOException e) {
            throw new StateException("state_permissions_failed", "Cannot restrict state file permissions", e);
        }
    }

    private static void tighten(Path target, boolean directory) throws IOException {
        PosixFileAttributeView posix = Files.getFileAttributeView(target, PosixFileAttributeView.class, LinkOption.NOFOLLOW_LINKS);
        if (posix != null) {
            posix.setPermissions(directory ? DIR_PERMS : FILE_PERMS);
            return;
        }
        AclFileAttributeView acl = Files.getFileAttributeView(target, AclFileAttributeView.class, LinkOption.NOFOLLOW_LINKS);
        FileOwnerAttributeView ownerView = Files.getFileAttributeView(target, FileOwnerAttributeView.class, LinkOption.NOFOLLOW_LINKS);
        if (acl == null || ownerView == null) {
            throw new StateException("state_permissions_unsupported",
                    "File system supports neither POSIX permissions nor ACLs; refusing to store state");
        }
        UserPrincipal owner = ownerView.getOwner();
        AclEntry.Builder entry = AclEntry.newBuilder()
                .setType(AclEntryType.ALLOW)
                .setPrincipal(owner)
                .setPermissions(EnumSet.allOf(AclEntryPermission.class));
        if (directory) {
            entry.setFlags(AclEntryFlag.FILE_INHERIT, AclEntryFlag.DIRECTORY_INHERIT);
        }
        // Replacing the ACL with an explicit list drops inherited entries for this object.
        acl.setAcl(List.of(entry.build()));
    }

    private static Path normalize(Path p) {
        Path abs = p.toAbsolutePath().normalize();
        // Resolve symlinks on the deepest existing ancestor so overlap checks see real locations.
        Path existing = abs;
        while (existing != null && !Files.exists(existing)) {
            existing = existing.getParent();
        }
        if (existing == null) {
            return abs;
        }
        try {
            return existing.toRealPath().resolve(existing.relativize(abs)).normalize();
        } catch (IOException e) {
            throw new StateException("state_dir_invalid", "Cannot resolve path", e);
        }
    }
}
