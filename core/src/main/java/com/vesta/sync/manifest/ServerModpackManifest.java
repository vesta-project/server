package com.vesta.sync.manifest;

import com.google.gson.annotations.SerializedName;

import java.util.Collections;
import java.util.List;

public final class ServerModpackManifest {
    public int manifestVersion = 1;
    public ModpackMode mode;
    public String source;
    @SerializedName("modpack_id")
    public String modpackId;
    @SerializedName("version_id")
    public String versionId;
    @SerializedName("download_url")
    public String downloadUrl;
    /** Relative path of a locally hosted archive under the server root. */
    @SerializedName("archive_path")
    public String archivePath;
    @SerializedName("archive_sha256")
    public String archiveSha256;
    @SerializedName("archive_size")
    public long archiveSize;
    /** {@code mrpack} or {@code zip} when {@link #mode} is {@link ModpackMode#ARCHIVE}. */
    @SerializedName("archive_format")
    public String archiveFormat;
    public String name;
    public String version;
    public long revision;
    public long minimumRevision;
    public String minecraftVersion;
    public ModloaderInfo modloader;
    public List<ManifestFile> mods = Collections.emptyList();
    public List<ManifestFile> overrides = Collections.emptyList();
    public ServerInfo serverInfo;
}
