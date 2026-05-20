package com.vesta.sync.protocol;

public final class FileRequest {
    public String type = "file_request";
    public long seq;
    public String session;
    public String file;
    public String sha256;
    public long size;
}
