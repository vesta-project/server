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
