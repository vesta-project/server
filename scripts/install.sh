#!/usr/bin/env bash
# Vesta Server Sync one-liner installer (Linux / macOS).
# Usage (production):
#   bash <(curl -fsSL https://raw.githubusercontent.com/vesta-project/server/dist/install.sh)
# Local / CI:
#   VESTA_DIST_BASE=file:///path/to/dist ./scripts/install.sh --root ./vesta-server --skip-port-check --no-service
#
# Does NOT install the Vesta connector mod/plugin. Vanilla servers are supported.

set -euo pipefail

DIST_BASE="${VESTA_DIST_BASE:-https://raw.githubusercontent.com/vesta-project/server/dist}"
REPO_HINT="https://github.com/vesta-project/server"
DEFAULT_ROOT="${PWD}/vesta-server"
ROOT=""
HTTP_PORT=9090
MC_PORT=25565
SKIP_PORT_CHECK=0
NO_SERVICE=0
MINECRAFT_CHOICE=""   # none|vanilla|paper|fabric|forge|neoforge|"" (prompt)
NONINTERACTIVE=0
TMPDIR_ROOT=""

log()  { printf '%s\n' "$*"; }
warn() { printf 'WARNING: %s\n' "$*" >&2; }
die()  { printf 'ERROR: %s\n' "$*" >&2; exit 1; }

usage() {
  cat <<EOF
Vesta Server installer

Options:
  --root <path>              Server root (default: ./vesta-server or prompt)
  --port <n>                 Vesta HTTP port (default: 9090)
  --minecraft <kind>         none|vanilla|paper|fabric|forge|neoforge
  --skip-port-check          Skip port-forward confirmation loop
  --no-service               Do not register systemd/launchd service
  --yes                      Non-interactive (use defaults / required flags)
  -h, --help                 Show this help
EOF
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --root) ROOT="${2:-}"; shift 2 ;;
    --port) HTTP_PORT="${2:-}"; shift 2 ;;
    --minecraft) MINECRAFT_CHOICE="${2:-}"; shift 2 ;;
    --skip-port-check) SKIP_PORT_CHECK=1; shift ;;
    --no-service) NO_SERVICE=1; shift ;;
    --yes|-y) NONINTERACTIVE=1; shift ;;
    -h|--help) usage; exit 0 ;;
    *) die "Unknown argument: $1" ;;
  esac
done

need_cmd() {
  command -v "$1" >/dev/null 2>&1 || die "Required command not found: $1"
}

fetch() {
  local url="$1" out="$2"
  if [[ "$url" == file://* ]]; then
    local path="${url#file://}"
    # Windows-style file:///C:/... or file:///path
    if [[ "$path" =~ ^/[A-Za-z]:/ ]]; then
      path="${path:1}"
    fi
    cp "$path" "$out"
    return
  fi
  if command -v curl >/dev/null 2>&1; then
    curl -fsSL "$url" -o "$out"
  elif command -v wget >/dev/null 2>&1; then
    wget -qO "$out" "$url"
  else
    die "Need curl or wget to download artifacts"
  fi
}

sha256_file() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{print $1}'
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | awk '{print $1}'
  else
    die "Need sha256sum or shasum"
  fi
}

check_java() {
  if ! command -v java >/dev/null 2>&1; then
    die "Java 17+ is required. Install a JDK, then re-run this installer."
  fi
  local ver
  ver="$(java -version 2>&1 | head -n1 || true)"
  log "Java: $ver"
}

prompt_root() {
  if [[ -n "$ROOT" ]]; then
    return
  fi
  if [[ "$NONINTERACTIVE" -eq 1 ]]; then
    ROOT="$DEFAULT_ROOT"
    return
  fi
  printf "Server root [%s]: " "$DEFAULT_ROOT"
  read -r answer || true
  ROOT="${answer:-$DEFAULT_ROOT}"
}

scaffold() {
  mkdir -p "$ROOT"/{mods,config,plugins,.vesta,vesta-wrapper}
  log "Scaffolded $ROOT (mods/, config/, plugins/, .vesta/)"
}

install_wrapper() {
  need_cmd unzip
  TMPDIR_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/vesta-install.XXXXXX")"
  trap 'rm -rf "$TMPDIR_ROOT"' EXIT

  local manifest_path="$TMPDIR_ROOT/manifest.json"
  local zip_path="$TMPDIR_ROOT/wrapper.zip"
  fetch "${DIST_BASE}/manifest.json" "$manifest_path"
  fetch "${DIST_BASE}/wrapper/latest.zip" "$zip_path"

  local expected
  expected="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1])).get("wrapperSha256",""))' "$manifest_path" 2>/dev/null \
    || grep -oE '"wrapperSha256"[[:space:]]*:[[:space:]]*"[^"]+"' "$manifest_path" | head -1 | sed 's/.*"\([^"]*\)"$/\1/')"
  if [[ -z "$expected" ]]; then
    die "manifest.json missing wrapperSha256"
  fi
  local actual
  actual="$(sha256_file "$zip_path")"
  if [[ "$actual" != "$expected" ]]; then
    die "wrapper zip SHA-256 mismatch (expected $expected, got $actual)"
  fi

  rm -rf "$ROOT/vesta-wrapper"
  mkdir -p "$ROOT/vesta-wrapper"
  unzip -q "$zip_path" -d "$ROOT/vesta-wrapper"
  # installDist zips often nest bin/lib under a single folder
  if [[ ! -x "$ROOT/vesta-wrapper/bin/wrapper" && ! -f "$ROOT/vesta-wrapper/bin/wrapper" ]]; then
    local nested
    nested="$(find "$ROOT/vesta-wrapper" -maxdepth 2 -type d -name bin -print -quit || true)"
    if [[ -n "$nested" ]]; then
      local parent
      parent="$(dirname "$nested")"
      shopt -s dotglob
      mv "$parent"/* "$ROOT/vesta-wrapper/" 2>/dev/null || true
      shopt -u dotglob
    fi
  fi
  chmod +x "$ROOT/vesta-wrapper/bin/wrapper" 2>/dev/null || true
  log "Installed wrapper into $ROOT/vesta-wrapper"
}

write_config() {
  local cfg="$ROOT/wrapper-config.toml"
  if [[ -f "$cfg" ]]; then
    log "Keeping existing wrapper-config.toml"
    return
  fi
  local defaults="$TMPDIR_ROOT/defaults.toml"
  if fetch "${DIST_BASE}/defaults/wrapper-config.toml" "$defaults" 2>/dev/null; then
    cp "$defaults" "$cfg"
  else
    cat > "$cfg" <<EOF
[server]
tcp-port = ${HTTP_PORT}
http-port = ${HTTP_PORT}
scan-root = "."
scan-paths = ["mods", "config"]

[server.public]
scheme = "https"
host = "localhost"
port = ${HTTP_PORT}

[server.tls]
enabled = true
auto-generate = true
keystore = ".vesta/tls.p12"
keystore-password = "vesta-generated"

[server.identity]
name = "Vesta Server"

[modpack]
name = "Server Modpack"
version = "1.0.0"
revision = 1
minimum-revision = 1
minecraft-version = "1.21.4"
source = "self-hosted"
unresolved-policy = "server"

[modpack.modloader]
type = "fabric"
version = "0.18.4"

[modpack.server]
host = "localhost"
port = ${MC_PORT}

[minecraft]
auto-start = false
launch-jar = "server.jar"
working-directory = "."
java-command = "java"
jvm-args = ["-Xms1G", "-Xmx2G"]
server-args = ["nogui"]

[connector]
auto-update = false

[security]
mode = "open"
password = ""
token-ttl-seconds = 86400
control-port = 9091
anonymous-level = 1
authenticated-level = 1
password-level = 4
linked-level = 1
whitelist-level = 3
EOF
  fi

  # Ensure ports and connector block match installer choices
  if command -v python3 >/dev/null 2>&1; then
    python3 - "$cfg" "$HTTP_PORT" "$MC_PORT" <<'PY'
import pathlib, sys
path = pathlib.Path(sys.argv[1])
http_port, mc_port = sys.argv[2], sys.argv[3]
text = path.read_text(encoding="utf-8")
if "[connector]" not in text:
    text += "\n[connector]\nauto-update = false\n"
path.write_text(text, encoding="utf-8")
PY
  fi
  log "Wrote $cfg (connector is NOT auto-installed)"
}

write_start_script() {
  cat > "$ROOT/start-vesta-wrapper.sh" <<EOF
#!/usr/bin/env bash
cd "\$(cd "\$(dirname "\$0")" && pwd)"
exec "\$PWD/vesta-wrapper/bin/wrapper" --config "\$PWD/wrapper-config.toml"
EOF
  chmod +x "$ROOT/start-vesta-wrapper.sh"
}

prompt_minecraft() {
  if [[ -n "$MINECRAFT_CHOICE" ]]; then
    return
  fi
  if [[ "$NONINTERACTIVE" -eq 1 ]]; then
    MINECRAFT_CHOICE="none"
    return
  fi
  cat <<EOF

Optional Minecraft server jar (Vesta connector mod/plugin is NEVER auto-installed):
  1) Skip (wrapper-only / attach later)
  2) Vanilla
  3) Paper
  4) Fabric
  5) Forge (manual — prints instructions)
  6) NeoForge (manual — prints instructions)
EOF
  printf "Choice [1]: "
  read -r choice || true
  case "${choice:-1}" in
    1|"") MINECRAFT_CHOICE="none" ;;
    2) MINECRAFT_CHOICE="vanilla" ;;
    3) MINECRAFT_CHOICE="paper" ;;
    4) MINECRAFT_CHOICE="fabric" ;;
    5) MINECRAFT_CHOICE="forge" ;;
    6) MINECRAFT_CHOICE="neoforge" ;;
    *) MINECRAFT_CHOICE="none" ;;
  esac
}

download_minecraft() {
  case "$MINECRAFT_CHOICE" in
    ""|none)
      log "Skipping Minecraft jar download"
      return
      ;;
    forge|neoforge)
      warn "Automatic $MINECRAFT_CHOICE install is not automated yet."
      log "Download the server installer from the official site, place the jar as server.jar,"
      log "then set [minecraft] launch-jar / auto-start in wrapper-config.toml."
      return
      ;;
    vanilla)
      need_cmd curl
      local manifest ver_url jar_url
      manifest="$TMPDIR_ROOT/mc-version_manifest.json"
      fetch "https://launchermeta.mojang.com/mc/game/version_manifest_v2.json" "$manifest"
      ver_url="$(python3 -c 'import json,sys; m=json.load(open(sys.argv[1])); print(next(v["url"] for v in m["versions"] if v["id"]=="1.21.4"))' "$manifest")"
      local ver_json="$TMPDIR_ROOT/mc-version.json"
      fetch "$ver_url" "$ver_json"
      jar_url="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["downloads"]["server"]["url"])' "$ver_json")"
      fetch "$jar_url" "$ROOT/server.jar"
      log "Downloaded vanilla server.jar"
      ;;
    paper)
      need_cmd curl
      local builds build
      builds="$TMPDIR_ROOT/paper-builds.json"
      fetch "https://api.papermc.io/v2/projects/paper/versions/1.21.4/builds" "$builds"
      build="$(python3 -c 'import json,sys; d=json.load(open(sys.argv[1])); print(d["builds"][-1]["build"])' "$builds")"
      fetch "https://api.papermc.io/v2/projects/paper/versions/1.21.4/builds/${build}/downloads/paper-1.21.4-${build}.jar" "$ROOT/server.jar"
      log "Downloaded Paper server.jar (build $build)"
      ;;
    fabric)
      need_cmd curl
      # Fabric server launcher jar
      fetch "https://meta.fabricmc.net/v2/versions/loader/1.21.4/0.16.9/1.0.1/server/jar" "$ROOT/server.jar" \
        || die "Could not download Fabric server jar"
      log "Downloaded Fabric server.jar"
      ;;
    *)
      die "Unknown --minecraft value: $MINECRAFT_CHOICE"
      ;;
  esac
}

port_forward_loop() {
  if [[ "$SKIP_PORT_CHECK" -eq 1 ]]; then
    log "Skipping port-forward confirmation (--skip-port-check)"
    return
  fi
  cat <<EOF

=== Port forwarding ===
Forward TCP ${HTTP_PORT} on your router/firewall to this machine so launchers
can reach Vesta from the internet.
EOF
  if [[ -f "$ROOT/server.jar" ]]; then
    log "If players join Minecraft directly, also forward TCP ${MC_PORT}."
  fi
  log "Local check: after start, open https://localhost:${HTTP_PORT}/devtools"
  log "Off-LAN check: from a phone on cellular, open https://YOUR_PUBLIC_IP:${HTTP_PORT}/devtools"
  if [[ "$NONINTERACTIVE" -eq 1 ]]; then
    warn "Non-interactive mode: not waiting for port confirmation."
    return
  fi
  while true; do
    printf "Have you forwarded TCP %s and verified access? [y/N/skip]: " "$HTTP_PORT"
    read -r ans || true
    case "$(printf '%s' "${ans:-}" | tr '[:upper:]' '[:lower:]')" in
      y|yes) log "Port forwarding confirmed."; break ;;
      skip|s) warn "Continuing without port confirmation."; break ;;
      *) log "Forward the port, then answer y — or type skip." ;;
    esac
  done
}

install_service_linux() {
  local unit_name="vesta-wrapper"
  local exec_start="$ROOT/vesta-wrapper/bin/wrapper --config $ROOT/wrapper-config.toml"
  local unit_body
  unit_body=$(cat <<EOF
[Unit]
Description=Vesta Server Sync Wrapper
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
WorkingDirectory=$ROOT
ExecStart=$ROOT/vesta-wrapper/bin/wrapper --config $ROOT/wrapper-config.toml
Restart=on-failure
RestartSec=5

[Install]
WantedBy=multi-user.target
EOF
)
  if [[ "$(id -u)" -eq 0 ]]; then
    printf '%s\n' "$unit_body" > "/etc/systemd/system/${unit_name}.service"
    systemctl daemon-reload
    systemctl enable --now "${unit_name}.service"
    log "Enabled systemd service ${unit_name}.service (system)"
  else
    mkdir -p "$HOME/.config/systemd/user"
    printf '%s\n' "$unit_body" | sed 's/WantedBy=multi-user.target/WantedBy=default.target/' \
      > "$HOME/.config/systemd/user/${unit_name}.service"
    systemctl --user daemon-reload
    systemctl --user enable --now "${unit_name}.service" || warn "Could not start user service; run: systemctl --user enable --now ${unit_name}"
    log "Enabled systemd user service ${unit_name}.service"
    log "Tip: loginctl enable-linger \$USER so it survives logout"
  fi
}

install_service_macos() {
  local label="com.vesta.wrapper"
  local plist
  if [[ "$(id -u)" -eq 0 ]]; then
    plist="/Library/LaunchDaemons/${label}.plist"
  else
    mkdir -p "$HOME/Library/LaunchAgents"
    plist="$HOME/Library/LaunchAgents/${label}.plist"
  fi
  cat > "$plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>${label}</string>
  <key>ProgramArguments</key>
  <array>
    <string>$ROOT/vesta-wrapper/bin/wrapper</string>
    <string>--config</string>
    <string>$ROOT/wrapper-config.toml</string>
  </array>
  <key>WorkingDirectory</key><string>$ROOT</string>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
  <key>StandardOutPath</key><string>$ROOT/.vesta/wrapper.out.log</string>
  <key>StandardErrorPath</key><string>$ROOT/.vesta/wrapper.err.log</string>
</dict>
</plist>
EOF
  launchctl bootout "gui/$(id -u)/${label}" 2>/dev/null || true
  launchctl bootstrap "gui/$(id -u)" "$plist" 2>/dev/null \
    || launchctl load -w "$plist" 2>/dev/null \
    || warn "Could not load launchd plist; load manually: launchctl load -w $plist"
  log "Installed launchd job $label ($plist)"
}

install_service() {
  if [[ "$NO_SERVICE" -eq 1 ]]; then
    log "Skipping OS service (--no-service)"
    return
  fi
  case "$(uname -s)" in
    Linux) install_service_linux ;;
    Darwin) install_service_macos ;;
    *) warn "No service installer for $(uname -s); use ./start-vesta-wrapper.sh" ;;
  esac
}

main() {
  check_java
  prompt_root
  ROOT="$(cd "$(dirname "$ROOT")" 2>/dev/null && pwd)/$(basename "$ROOT")"
  # create parent then resolve
  mkdir -p "$ROOT"
  ROOT="$(cd "$ROOT" && pwd)"

  log "Installing Vesta into: $ROOT"
  log "Dist base: $DIST_BASE"
  scaffold
  install_wrapper
  write_config
  write_start_script
  prompt_minecraft
  download_minecraft
  port_forward_loop
  install_service

  cat <<EOF

Done.
  Start manually:  $ROOT/start-vesta-wrapper.sh
  Devtools:        https://localhost:${HTTP_PORT}/devtools
  Docs:            ${REPO_HINT}

The Vesta connector mod/plugin was NOT installed. For non-vanilla servers,
install it later from GitHub Releases / panel one-click when available.
Set connector.auto-update = true only when you intentionally opt in later.
EOF
}

main
