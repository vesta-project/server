package com.vesta.sync.manifest;

import com.google.gson.annotations.SerializedName;

public enum ModpackMode {
    @SerializedName("platform") PLATFORM,
    @SerializedName("archive") ARCHIVE,
    @SerializedName("custom") CUSTOM
}
