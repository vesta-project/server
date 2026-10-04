package com.vesta.sync.manifest;

import com.google.gson.JsonArray;
import com.google.gson.JsonObject;
import com.google.gson.JsonParser;
import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.io.TempDir;

import java.io.InputStream;
import java.io.InputStreamReader;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.ArrayList;
import java.util.Collections;
import java.util.zip.ZipFile;

import static org.junit.jupiter.api.Assertions.*;

class VestaPackInteroperabilityTest {
    @TempDir Path root;

    @Test
    void javaBuilderMatchesTheLauncherWireFixtureIncludingProviderAliasesAndHashes() throws Exception {
        JsonObject expected;
        try (InputStream resource = getClass().getResourceAsStream("/vesta-builder-v1.json")) {
            assertNotNull(resource);
            expected = JsonParser.parseReader(new InputStreamReader(resource, StandardCharsets.UTF_8)).getAsJsonObject();
        }
        ServerModpackManifest manifest = new ServerModpackManifest();
        manifest.name = expected.get("name").getAsString();
        manifest.version = expected.get("version").getAsString();
        manifest.revision = expected.get("revision").getAsLong();
        manifest.minimumRevision = expected.get("minimumRevision").getAsLong();
        manifest.minecraftVersion = expected.get("minecraftVersion").getAsString();
        manifest.modloader = new ModloaderInfo();
        manifest.modloader.type = expected.get("modloaderType").getAsString();
        manifest.modloader.version = expected.get("modloaderVersion").getAsString();
        manifest.mods = new ArrayList<ManifestFile>();
        manifest.overrides = Collections.emptyList();
        JsonArray files = expected.getAsJsonArray("files");
        for (int i = 0; i < files.size(); i++) {
            JsonObject wire = files.get(i).getAsJsonObject();
            ManifestFile file = new ManifestFile();
            file.path = wire.get("path").getAsString();
            file.size = wire.get("size").getAsLong();
            file.sha1 = wire.getAsJsonObject("hashes").get("sha1").getAsString();
            file.sha256 = wire.getAsJsonObject("hashes").get("sha256").getAsString();
            JsonObject source = wire.getAsJsonObject("source");
            file.source = new FileSource();
            file.source.type = source.get("type").getAsString();
            if (source.has("project_id")) {
                file.source.platform = source.get("platform").getAsString();
                file.source.projectId = source.get("project_id").getAsString();
                file.source.versionId = source.get("version_id").getAsString();
                file.source.url = source.get("url").getAsString();
            } else {
                Path embedded = root.resolve(file.path);
                Files.createDirectories(embedded.getParent());
                Files.write(embedded, "embedded".getBytes(StandardCharsets.UTF_8));
            }
            manifest.mods.add(file);
        }
        LocalArchivePack pack = VestaPackBuilder.build(root, manifest, root.resolve("fixture.vestapack"));
        try (ZipFile archive = new ZipFile(pack.resolvedPath.toFile());
             InputStream index = archive.getInputStream(archive.getEntry(VestaPackBuilder.INDEX_NAME))) {
            JsonObject actual = JsonParser.parseReader(new InputStreamReader(index, StandardCharsets.UTF_8)).getAsJsonObject();
            assertEquals(expected, actual, "Keep the identical fixture in piston-lib/tests/fixtures in sync");
            assertNotNull(archive.getEntry("overrides/config/options.txt"));
            assertNull(archive.getEntry("overrides/mods/modrinth.jar"));
            assertNull(archive.getEntry("overrides/mods/curseforge.jar"));
        }
    }
}
