# Testing server connections

Run the server communication suite from the repository root:

```powershell
.\gradlew.bat testServerConnections
```

The suite starts Jetty on an ephemeral loopback port and exercises real HTTP and WebSocket requests. Mojang verification is replaced by the same `SessionVerifier` boundary used by production challenge authentication, so no Minecraft account, secret, or external network connection is required.

Coverage includes challenge expiry and replay, profile and server binding, bearer expiry and revocation, realtime ticket expiry and single use, public descriptors, capability-gated manifests and downloads, removal of direct token submission, realtime authentication order, ping/pong, and ticket replay rejection.

CI workflows are intentionally not included yet; this command is the local verification entry point.
