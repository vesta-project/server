package com.vesta.sync.manifest;

import com.google.gson.Gson;
import com.google.gson.GsonBuilder;
import com.google.gson.annotations.SerializedName;
import com.vesta.sync.hashing.FileHasher;
import org.slf4j.Logger;
import org.slf4j.LoggerFactory;

import java.io.BufferedOutputStream;
import java.io.IOException;
import java.io.OutputStreamWriter;
import java.io.Writer;
import java.nio.charset.StandardCharsets;
import java.nio.file.AtomicMoveNotSupportedException;
import java.nio.file.Files;
import java.nio.file.Path;
import java.nio.file.StandardCopyOption;
import java.util.ArrayList;
import java.util.Collections;
import java.util.Comparator;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import java.util.zip.Deflater;
import java.util.zip.ZipEntry;
import java.util.zip.ZipOutputStream;

/**
 * Builds a hybrid Vesta pack archive ({@code vesta.index.json} + embedded overrides).
 *
 * <p>Known Modrinth/CurseForge files stay as external references. Unknown/server-hosted
 * public files are embedded under {@code overrides/}. Generation is deterministic and
 * written atomically (temp file then replace).
 */
public final class VestaPackBuilder {

    public static final String INDEX_NAME = "vesta.index.json";
    public static final String FORMAT = "vesta";
    public static final int FORMAT_VERSION = 1;

    private static final Logger log = LoggerFactory.getLogger(VestaPackBuilder.class);
    private static final Gson GSON = new GsonBuilder().disableHtmlEscaping().create();

    private VestaPackBuilder() {}

    public static LocalArchivePack build(
            Path scanRoot,
            ServerModpackManifest manifest,
            Path outputPath
    ) throws IOException {
        if (scanRoot == null || manifest == null || outputPath == null) {
            throw new IllegalArgumentException("scanRoot, manifest, and outputPath are required");
        }

        Path root = scanRoot.toAbsolutePath().normalize();
        Path output = outputPath.toAbsolutePath().normalize();
        Files.createDirectories(output.getParent());

        List<ManifestFile> entries = collectEntries(manifest);
        Index index = new Index();
        index.formatVersion = FORMAT_VERSION;
        index.name = manifest.name;
        index.version = manifest.version;
        index.revision = manifest.revision;
        index.minimumRevision = manifest.minimumRevision;
        index.minecraftVersion = manifest.minecraftVersion;
        if (manifest.modloader != null) {
            index.modloaderType = manifest.modloader.type;
            index.modloaderVersion = manifest.modloader.version;
        }
        index.dependencies = buildDependencies(manifest);
        index.files = new ArrayList<IndexFile>();

        Path temp = output.resolveSibling(output.getFileName().toString() + ".tmp");
        Files.deleteIfExists(temp);

        try (ZipOutputStream zip = new ZipOutputStream(
                new BufferedOutputStream(Files.newOutputStream(temp)))) {
            zip.setLevel(Deflater.DEFAULT_COMPRESSION);

            for (ManifestFile entry : entries) {
                IndexFile file = toIndexFile(entry);
                index.files.add(file);
                if (isEmbedded(entry)) {
                    Path live = root.resolve(entry.path).normalize();
                    if (!live.startsWith(root) || !Files.isRegularFile(live)) {
                        throw new IOException("Missing embedded pack file: " + entry.path);
                    }
                    ZipEntry zipEntry = new ZipEntry("overrides/" + entry.path);
                    zip.putNextEntry(zipEntry);
                    Files.copy(live, zip);
                    zip.closeEntry();
                }
            }

            // Stable order for the index itself.
            Collections.sort(index.files, new Comparator<IndexFile>() {
                @Override
                public int compare(IndexFile a, IndexFile b) {
                    return a.path.compareTo(b.path);
                }
            });

            ZipEntry indexEntry = new ZipEntry(INDEX_NAME);
            zip.putNextEntry(indexEntry);
            Writer writer = new OutputStreamWriter(zip, StandardCharsets.UTF_8);
            GSON.toJson(index, writer);
            writer.flush();
            zip.closeEntry();
        } catch (IOException e) {
            Files.deleteIfExists(temp);
            throw e;
        }

        try {
            Files.move(temp, output, StandardCopyOption.REPLACE_EXISTING, StandardCopyOption.ATOMIC_MOVE);
        } catch (AtomicMoveNotSupportedException e) {
            Files.move(temp, output, StandardCopyOption.REPLACE_EXISTING);
        }
        String sha256 = FileHasher.sha256(output);
        long size = Files.size(output);
        log.info("Generated Vesta pack at {} ({} bytes, {} files)", output, size, index.files.size());
        return LocalArchivePack.of(
                output,
                output.getFileName().toString(),
                sha256,
                size,
                FORMAT
        );
    }

    private static List<ManifestFile> collectEntries(ServerModpackManifest manifest) {
        List<ManifestFile> entries = new ArrayList<ManifestFile>();
        if (manifest.mods != null) {
            entries.addAll(manifest.mods);
        }
        if (manifest.overrides != null) {
            entries.addAll(manifest.overrides);
        }
        Collections.sort(entries, new Comparator<ManifestFile>() {
            @Override
            public int compare(ManifestFile a, ManifestFile b) {
                return a.path.compareTo(b.path);
            }
        });
        return entries;
    }

    private static Map<String, String> buildDependencies(ServerModpackManifest manifest) {
        Map<String, String> dependencies = new LinkedHashMap<String, String>();
        if (manifest.minecraftVersion != null) {
            dependencies.put("minecraft", manifest.minecraftVersion);
        }
        if (manifest.modloader != null && manifest.modloader.type != null) {
            String type = manifest.modloader.type.toLowerCase();
            String version = manifest.modloader.version != null ? manifest.modloader.version : "";
            if ("fabric".equals(type)) {
                dependencies.put("fabric-loader", version);
            } else if ("quilt".equals(type)) {
                dependencies.put("quilt-loader", version);
            } else if ("forge".equals(type)) {
                dependencies.put("forge", version);
            } else if ("neoforge".equals(type)) {
                dependencies.put("neoforge", version);
            }
        }
        return dependencies;
    }

    private static boolean isEmbedded(ManifestFile entry) {
        if (entry.source == null || entry.source.type == null) {
            return true;
        }
        String type = entry.source.type.toLowerCase();
        return !"modrinth".equals(type) && !"curseforge".equals(type);
    }

    private static IndexFile toIndexFile(ManifestFile entry) {
        IndexFile file = new IndexFile();
        file.path = entry.path;
        file.size = entry.size;
        file.hashes = new LinkedHashMap<String, String>();
        if (entry.sha256 != null) {
            file.hashes.put("sha256", entry.sha256);
        }
        if (entry.sha1 != null) {
            file.hashes.put("sha1", entry.sha1);
        }
        file.env = new Env();
        file.env.client = "required";
        file.env.server = "required";
        file.source = new Source();
        if (isEmbedded(entry)) {
            file.source.type = "embedded";
        } else {
            file.source.type = entry.source.type;
            file.source.platform = entry.source.platform != null
                    ? entry.source.platform
                    : entry.source.type;
            file.source.projectId = entry.source.projectId;
            file.source.versionId = entry.source.versionId;
            file.source.url = entry.source.url;
        }
        return file;
    }

    static final class Index {
        int formatVersion;
        String name;
        String version;
        long revision;
        long minimumRevision;
        String minecraftVersion;
        String modloaderType;
        String modloaderVersion;
        Map<String, String> dependencies;
        List<IndexFile> files;
    }

    static final class IndexFile {
        String path;
        long size;
        Map<String, String> hashes;
        Env env;
        Source source;
    }

    static final class Env {
        String client;
        String server;
    }

    static final class Source {
        String type;
        String platform;
        @SerializedName("project_id")
        String projectId;
        @SerializedName("version_id")
        String versionId;
        String url;
    }
}
