# Demo server

Run the wrapper from this directory so the relative scan, TLS, and optional
Minecraft launch paths resolve correctly:

```powershell
cd V:\server-connections-mvp\demo-server
..\gradlew.bat :wrapper:run --args="--config wrapper-config.toml"
```

The default configuration serves the tracked `config/` and `mods/` demo files.
Connect the launcher using `localhost` as the Minecraft address and
`https://localhost:9090` under **Connection details** when discovery cannot find
the local Vesta endpoint automatically.

To test a Modrinth-referenced pack, edit `[modpack.platform]` in
`wrapper-config.toml`:

```toml
source = "modrinth"
project-id = "the-project-id"
version-id = "the-exact-version-id"
download-url = ""
```

Restart the wrapper, forget or reconnect the saved server in the launcher, and
create a new instance. The launcher should fetch the `.mrpack` from Modrinth;
the wrapper will not host those pack files.

To test wrapper-managed Minecraft startup, place the server jar in this folder,
set `minecraft.launch-jar` to its filename, accept the Minecraft EULA, and set
`minecraft.auto-start = true`.
