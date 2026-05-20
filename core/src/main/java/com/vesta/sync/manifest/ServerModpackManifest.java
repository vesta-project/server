package com.vesta.sync.manifest;

import java.util.Collections;
import java.util.List;

public final class ServerModpackManifest {
    public int manifestVersion = 1;
    public ModpackMode mode;
    public String name;
    public String version;
    public String minecraftVersion;
    public ModloaderInfo modloader;
    public List<ManifestFile> mods = Collections.emptyList();
    public List<ManifestFile> overrides = Collections.emptyList();
    public ServerInfo serverInfo;
}
