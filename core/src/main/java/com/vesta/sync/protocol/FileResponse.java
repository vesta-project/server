package com.vesta.sync.protocol;

public final class FileResponse {
    public String type = "file_response";
    public long seq;
    public String session;
    public String ts;
    public String file;
    public String sha256;
    public String method;
    public String url;
    public String dataBase64;
}
