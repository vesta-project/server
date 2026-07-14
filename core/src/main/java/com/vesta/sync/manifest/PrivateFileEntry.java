package com.vesta.sync.manifest;

/**
 * Metadata for a file served only via the authenticated private API.
 */
public final class PrivateFileEntry {
    public String path;
    public String sha256;
    public String sha1;
    public long size;
}
