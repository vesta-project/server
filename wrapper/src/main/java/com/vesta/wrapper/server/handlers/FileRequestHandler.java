package com.vesta.wrapper.server.handlers;

import com.vesta.sync.protocol.FileRequest;
import com.vesta.sync.protocol.FileResponse;
import com.vesta.sync.protocol.TcpMessageCodec;
import com.vesta.sync.util.PublicEndpoint;
import com.vesta.wrapper.server.ClientConnection;
import org.slf4j.Logger;
import org.slf4j.LoggerFactory;

import java.io.IOException;

public final class FileRequestHandler implements com.vesta.wrapper.server.MessageRouter.MessageHandler {

    private static final Logger log = LoggerFactory.getLogger(FileRequestHandler.class);
    private final PublicEndpoint endpoint;

    public FileRequestHandler(PublicEndpoint endpoint) {
        this.endpoint = endpoint;
    }

    @Override
    public void handle(ClientConnection client, Object message) throws IOException {
        FileRequest fr = (FileRequest) message;
        log.info("FileRequest: {}, sha256={}", fr.file, fr.sha256);

        FileResponse response = new FileResponse();
        response.seq = fr.seq;
        response.session = fr.session;
        response.ts = TcpMessageCodec.now();
        response.file = fr.file;
        response.sha256 = fr.sha256;
        response.method = "url";
        response.url = endpoint.fileUrl(fr.sha256);

        client.send(response);
    }
}
