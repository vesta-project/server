package com.vesta.sync.util;

import org.junit.jupiter.api.Test;

import static org.junit.jupiter.api.Assertions.*;

class PublicEndpointTest {

    @Test
    void buildsFileAndBaseUrls() {
        PublicEndpoint endpoint = new PublicEndpoint("http", "play.example.com", 9090);
        assertEquals("http://play.example.com:9090/api/v1/files/deadbeef", endpoint.fileUrl("deadbeef"));
        assertEquals("http://play.example.com:9090", endpoint.baseUrl());
    }

    @Test
    void defaultsMissingValues() {
        PublicEndpoint endpoint = new PublicEndpoint("", "", 9090);
        assertTrue(endpoint.baseUrl().startsWith("http://localhost:9090"));
    }
}
