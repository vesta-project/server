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
INSTANCE=""         # instance slug (for service/task + registry)
EXISTING_MODE=0     # treat $ROOT as an existing Minecraft server
BACKUP=0            # create a timestamped backup zip before mutating install
I_UNDERSTAND_RISK=0 # allow mutating install without --backup
UNINSTALL=0         # delegate to vesta-wrapperctl uninstall
MINECRAFT_AUTO_START=0 # override minecraft.auto-start (existing server mode)
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
  --instance <name>         Instance slug (default: server root folder name)
  --existing                 Treat --root as an existing Minecraft server
  --backup                   Backup mods/, config/, and wrapper-config.toml before mutating
  --i-understand-risk        Allow mutating install without --backup
  --uninstall                Delegate uninstall to vesta-wrapperctl uninstall
  --minecraft <kind>         none|vanilla|paper|fabric|forge|neoforge
  --minecraft-auto-start    For --existing: set minecraft.auto-start=true
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
    --instance) INSTANCE="${2:-}"; shift 2 ;;
    --existing) EXISTING_MODE=1; shift ;;
    --backup) BACKUP=1; shift ;;
    --i-understand-risk) I_UNDERSTAND_RISK=1; shift ;;
    --uninstall) UNINSTALL=1; shift ;;
    --minecraft-auto-start) MINECRAFT_AUTO_START=1; shift ;;
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

slugify_instance() {
  local s="$1"
  # Keep it simple + predictable for unit/label names.
  printf '%s' "$s" \
    | tr '[:upper:]' '[:lower:]' \
    | sed -E 's/[^a-z0-9]+/-/g; s/^-+//; s/-+$//' \
    | sed -E 's/--+/-/g'
}

get_instance_slug() {
  if [[ -n "$INSTANCE" ]]; then
    slugify_instance "$INSTANCE"
  else
    slugify_instance "$(basename "$ROOT")"
  fi
}

registry_path() {
  # Shared contract with vesta-wrapperctl.
  printf '%s/.vesta/instances.json' "$HOME"
}

port_is_busy() {
  local port="$1"
  python3 - "$port" <<'PY'
import socket, sys
port = int(sys.argv[1])
s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
s.settimeout(0.25)
try:
    s.connect(("127.0.0.1", port))
    print("1")
except Exception:
    print("0")
finally:
    s.close()
PY
}

choose_free_port_pair() {
  local http_port="$1"
  local control_port="$2"
  local tries=0
  while true; do
    local http_busy control_busy
    http_busy="$(port_is_busy "$http_port")"
    control_busy="$(port_is_busy "$control_port")"
    if [[ "$http_busy" == "0" && "$control_busy" == "0" ]]; then
      printf '%s %s\n' "$http_port" "$control_port"
      return
    fi

    tries=$((tries + 1))
    if [[ "$tries" -gt 100 ]]; then
      die "Could not find free HTTP/control ports after many attempts (last: $http_port/$control_port)"
    fi

    if [[ "$NONINTERACTIVE" -eq 1 ]]; then
      warn "Ports busy on localhost ($http_port/$control_port). Trying next pair..."
      http_port=$((http_port + 1))
      control_port=$((http_port + 1))
      continue
    fi

    printf "Port conflict detected on localhost for HTTP=%s / control=%s.\nTry next free pair starting at %s? [Y/n]: " \
      "$http_port" "$control_port" "$((http_port+1))"
    read -r ans || true
    ans="${ans:-Y}"
    case "$(printf '%s' "$ans" | tr '[:upper:]' '[:lower:]')" in
      n|no) die "Aborting due to port conflict." ;;
      *) http_port=$((http_port + 1)); control_port=$((http_port + 1)) ;;
    esac
  done
}

detect_existing_launch_jar() {
  # Wrapper can start/stop the server process only if the launch-jar matches.
  if [[ -f "$ROOT/server.jar" ]]; then
    printf '%s\n' "server.jar"
    return
  fi

  # Common heuristics: pick the newest jar matching typical server names.
  local newest=""
  newest="$(ls -t "$ROOT"/*.jar 2>/dev/null | grep -E 'server\.jar|paper.*\.jar|fabric.*\.jar|forge.*\.jar|neoforge.*\.jar' | grep -v -E 'sources\.jar|javadoc\.jar' | head -n1 || true)"
  if [[ -n "$newest" && -f "$newest" ]]; then
    basename "$newest"
    return
  fi

  # Fallback to defaults; minecraft.auto-start stays false unless opted in.
  printf '%s\n' "server.jar"
}

extract_wrapper_config_fields() {
  local cfg="$1"
  python3 - "$cfg" <<'PY'
import re, sys
cfg = sys.argv[1]
http_port = 0
control_port = 0
connector_auto_update = None
identity_id = None
minecraft_auto_start = None
minecraft_launch_jar = None

section = None
try:
    lines = open(cfg, "r", encoding="utf-8").read().splitlines()
except FileNotFoundError:
    print("0 0 0 0 0 server.jar")
    raise SystemExit(0)

for line in lines:
    m = re.match(r'^\s*\[([^\]]+)\]\s*$', line)
    if m:
        section = m.group(1).strip()
        continue
    if line.strip().startswith("#"):
        continue

    if section == "server":
        m = re.match(r'^\s*(tcp-port|http-port)\s*=\s*(\d+)\s*$', line)
        if m and m.group(1) == "http-port":
            http_port = int(m.group(2))
    elif section == "security":
        m = re.match(r'^\s*control-port\s*=\s*(\d+)\s*$', line)
        if m:
            control_port = int(m.group(1))
    elif section == "connector":
        m = re.match(r'^\s*auto-update\s*=\s*(true|false)\s*$', line)
        if m:
            connector_auto_update = (m.group(1) == "true")
    elif section == "server.identity":
        m = re.match(r'^\s*id\s*=\s*"?([^"]+)"?\s*$', line)
        if m:
            identity_id = m.group(1)
    elif section == "minecraft":
        m = re.match(r'^\s*auto-start\s*=\s*(true|false)\s*$', line)
        if m:
            minecraft_auto_start = (m.group(1) == "true")
        m = re.match(r'^\s*launch-jar\s*=\s*"?([^"]+)"?\s*$', line)
        if m:
            minecraft_launch_jar = m.group(1)

has_identity_id = 1 if identity_id else 0
if connector_auto_update is None:
    connector_auto_update = False
if minecraft_auto_start is None:
    minecraft_auto_start = False
if not minecraft_launch_jar:
    minecraft_launch_jar = "server.jar"

print(f"{http_port} {control_port} {has_identity_id} {1 if connector_auto_update else 0} {1 if minecraft_auto_start else 0} {minecraft_launch_jar}")
PY
}

create_backup() {
  local ts
  ts="$(date -u +"%Y%m%dT%H%M%SZ")"
  local out="$ROOT/vesta-backup-$ts.zip"
  if [[ -d "$ROOT/vesta-wrapper" || -f "$ROOT/wrapper-config.toml" ]]; then
    :
  fi

  if command -v zip >/dev/null 2>&1; then
    # Backup only the operator-relevant state; wrapper overwrite is handled separately.
    rm -f "$out" 2>/dev/null || true
    (cd "$ROOT" && zip -qr "$out" mods config wrapper-config.toml >/dev/null 2>&1 || true)
    log "Backup created: $out"
  else
    # Fallback: tar.gz when zip isn't available.
    out="$ROOT/vesta-backup-$ts.tar.gz"
    tar -czf "$out" -C "$ROOT" mods config wrapper-config.toml 2>/dev/null || true
    log "Backup created: $out (tar.gz fallback)"
  fi
}

toml_server_identity_id() {
  local cfg="$1"
  python3 - "$cfg" <<'PY'
import re, sys
cfg=sys.argv[1]
section=None
identity_id=None
for line in open(cfg, "r", encoding="utf-8"):
    m=re.match(r'^\s*\[([^\]]+)\]\s*$', line)
    if m: section=m.group(1).strip(); continue
    if line.strip().startswith("#"): continue
    if section=="server.identity":
        m=re.match(r'^\s*id\s*=\s*"?([^"]+)"?\s*$', line)
        if m:
            identity_id=m.group(1); break
print(identity_id or "")
PY
}

registry_upsert() {
  local instance_name="$1"
  local cfg_path="$2"
  local root_path="$3"
  local server_id="$4"
  local service_name="$5"
  local reg_path
  reg_path="$(registry_path)"

  mkdir -p "$(dirname "$reg_path")"
  local tmp="${reg_path}.tmp.$$"

  python3 - "$reg_path" "$cfg_path" "$root_path" "$server_id" "$instance_name" "$service_name" <<'PY'
import json, os, sys, datetime, tempfile
reg_path = sys.argv[1]
cfg_path = sys.argv[2]
root_path = sys.argv[3]
server_id = sys.argv[4]
instance_name = sys.argv[5]
service_name = sys.argv[6]
now = datetime.datetime.utcnow().replace(microsecond=0).isoformat() + "Z"

data = {}
if os.path.exists(reg_path):
    try:
        data = json.load(open(reg_path, "r", encoding="utf-8"))
    except Exception:
        data = {}

data.setdefault("version", 1)
data.setdefault("instances", [])

instances = data["instances"]
if not isinstance(instances, list):
    instances = []

updated = False
for inst in instances:
    if inst.get("config") == cfg_path:
        # Ports are identity/discovery only; remove any legacy port fields.
        inst.pop("http_port", None)
        inst.pop("control_port", None)
        inst["name"] = instance_name
        inst["root"] = root_path
        if server_id:
            inst["server_id"] = server_id
        inst["service_name"] = service_name
        updated = True
        break

if not updated:
    instances.append({
        "name": instance_name,
        "config": cfg_path,
        "root": root_path,
        "server_id": server_id if server_id else "",
        "service_name": service_name,
        "registered_at": now,
    })

# Remove empty server_id field to keep it truly optional.
for inst in instances:
    if "server_id" in inst and not inst["server_id"]:
        inst.pop("server_id", None)

data["instances"] = instances

tmp_path = reg_path + ".tmp"
with open(tmp_path, "w", encoding="utf-8") as f:
    json.dump(data, f, indent=2, ensure_ascii=True)
os.replace(tmp_path, reg_path)
PY
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
  local defaults="$TMPDIR_ROOT/defaults.toml"
  if [[ -f "$cfg" ]]; then
    log "Patching existing wrapper-config.toml"
  else
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
control-port = ${CONTROL_PORT}
anonymous-level = 1
authenticated-level = 1
password-level = 4
linked-level = 1
whitelist-level = 3
EOF
    fi
  fi

  # Patch: ports, connector auto-update=false, identity id, and minecraft auto-start/launch-jar.
  # We keep this logic here (instead of storing ports in registry) so instances only differ by wrapper-config.
  python3 - "$cfg" "$HTTP_PORT" "$CONTROL_PORT" "$MINECRAFT_AUTO_START" "$MINECRAFT_LAUNCH_JAR" <<'PY'
import re, sys, uuid, pathlib
path = pathlib.Path(sys.argv[1])
http_port = int(sys.argv[2])
control_port = int(sys.argv[3])
auto_start = sys.argv[4].strip().lower() in ("1","true","yes","y","on")
launch_jar = sys.argv[5].strip()
if not launch_jar:
    launch_jar = None

text = path.read_text(encoding="utf-8")
lines = text.splitlines(True)

section = None
has_identity_id = False
for line in lines:
    stripped = line.lstrip()
    if stripped.startswith("id") and not stripped.startswith("#"):
        # inside [server.identity] we only care about actual active `id = ...`
        pass

# quick presence check for active id line
for line in lines:
    s = line.strip()
    if re.match(r'^[^#]*id\s*=\s*".+?"\s*$', s):
        has_identity_id = True
        break

out = []
section = None
for line in lines:
    m = re.match(r'^\s*\[([^\]]+)\]\s*$', line)
    if m:
        section = m.group(1).strip()

    if section == "server" and re.match(r'^\s*(tcp-port|http-port)\s*=\s*\d+\s*$', line):
        key = re.match(r'^\s*(tcp-port|http-port)\s*=\s*', line).group(1)
        indent = re.match(r'^(\s*)', line).group(1)
        out.append(f"{indent}{key} = {http_port}\n")
        continue

    if section == "server.public" and re.match(r'^\s*port\s*=\s*\d+\s*$', line):
        indent = re.match(r'^(\s*)', line).group(1)
        out.append(f"{indent}port = {http_port}\n")
        continue

    if section == "security" and re.match(r'^\s*control-port\s*=\s*\d+\s*$', line):
        indent = re.match(r'^(\s*)', line).group(1)
        out.append(f"{indent}control-port = {control_port}\n")
        continue

    if section == "connector" and re.match(r'^\s*auto-update\s*=\s*(true|false)\s*$', line):
        indent = re.match(r'^(\s*)', line).group(1)
        out.append(f"{indent}auto-update = false\n")
        continue

    if section == "minecraft" and re.match(r'^\s*auto-start\s*=\s*(true|false)\s*$', line):
        indent = re.match(r'^(\s*)', line).group(1)
        out.append(f"{indent}auto-start = {'true' if auto_start else 'false'}\n")
        continue

    if section == "minecraft" and launch_jar is not None and re.match(r'^\s*launch-jar\s*=\s*.*$', line):
        indent = re.match(r'^(\s*)', line).group(1)
        out.append(f'{indent}launch-jar = "{launch_jar}"\n')
        continue

    out.append(line)

out_text = "".join(out)

# Ensure connector section exists.
if "[connector]" not in out_text:
    out_text += "\n[connector]\nauto-update = false\n"

# Ensure server.identity.id exists (for stable identity).
if not has_identity_id:
    new_id = str(uuid.uuid4())
    # Insert right after the `[server.identity]` header.
    out_text = re.sub(
        r'(\[server\.identity\]\s*\n)',
        r'\1id = "%s"\n' % new_id,
        out_text,
        count=1
    )

path.write_text(out_text, encoding="utf-8")
PY

  log "Wrote/Patched $cfg (connector auto-update=false)"
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
  if [[ -n "$MINECRAFT_LAUNCH_JAR" && -f "$ROOT/$MINECRAFT_LAUNCH_JAR" ]]; then
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
  local unit_name="vesta-wrapper-${INSTANCE}"
  local exec_start="$ROOT/vesta-wrapper/bin/wrapper --config $ROOT/wrapper-config.toml"
  local unit_body
  unit_body=$(cat <<EOF
[Unit]
Description=Vesta Server Sync Wrapper (${INSTANCE})
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
  local label="com.vesta.wrapper.${INSTANCE}"
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

http_get_status() {
  # TLS is intentionally self-signed in defaults; ignore cert during installer verification.
  if command -v curl >/dev/null 2>&1; then
    curl -k -fsS "https://127.0.0.1:${HTTP_PORT}/api/v1/status" >/dev/null 2>&1
    return $?
  fi

  python3 - <<PY >/dev/null 2>&1
import ssl, urllib.request
ctx = ssl._create_unverified_context()
url = f"https://127.0.0.1:{HTTP_PORT}/api/v1/status"
with urllib.request.urlopen(url, context=ctx, timeout=2) as r:
    r.read()
PY
}

wait_for_status() {
  local tries=30
  local delay=1
  while [[ $tries -gt 0 ]]; do
    if http_get_status; then
      return 0
    fi
    tries=$((tries - 1))
    sleep "$delay"
  done
  return 1
}

ensure_wrapper_running_for_verify() {
  # If service install is disabled, start the wrapper briefly just for verification.
  if wait_for_status; then
    return 0
  fi

  if [[ "$NO_SERVICE" -ne 1 ]]; then
    # Assume the service will come up; caller can warn on failure.
    return 1
  fi

  log "No-service mode: starting wrapper briefly to verify /api/v1/status..."
  need_cmd bash
  "$ROOT/vesta-wrapper/bin/wrapper" --config "$ROOT/wrapper-config.toml" \
    > "$ROOT/.vesta/verify.out.log" 2> "$ROOT/.vesta/verify.err.log" &
  local pid=$!

  # Give it a moment before re-checking.
  sleep 2
  if wait_for_status; then
    kill "$pid" >/dev/null 2>&1 || true
    return 0
  fi

  kill "$pid" >/dev/null 2>&1 || true
  return 1
}

main() {
  check_java
  prompt_root
  ROOT="$(cd "$(dirname "$ROOT")" 2>/dev/null && pwd)/$(basename "$ROOT")"

  # create parent then resolve
  mkdir -p "$ROOT"
  ROOT="$(cd "$ROOT" && pwd)"

  INSTANCE="$(get_instance_slug)"
  if [[ -z "$INSTANCE" ]]; then
    die "Could not determine --instance slug (provide --instance explicitly)"
  fi
  # service_name must match the OS lifecycle identifier (systemd unit / launchd label).
  local service_name
  case "$(uname -s)" in
    Darwin) service_name="com.vesta.wrapper.${INSTANCE}" ;;
    *) service_name="vesta-wrapper-${INSTANCE}" ;;
  esac

  if [[ "$UNINSTALL" -eq 1 ]]; then
    if ! command -v vesta-wrapperctl >/dev/null 2>&1; then
      die "vesta-wrapperctl not found. Install/enable it (Wave 2) to use --uninstall."
    fi
    log "Delegating uninstall to vesta-wrapperctl (instance: $INSTANCE)..."
    vesta-wrapperctl uninstall --instance "$INSTANCE"
    return
  fi

  log "Installing Vesta into: $ROOT"
  log "Instance: $INSTANCE"
  log "Dist base: $DIST_BASE"

  # Existing-server mode (optional): allow interactive detection for operators who point at an existing server root.
  if [[ "$EXISTING_MODE" -ne 1 ]]; then
    if [[ -f "$ROOT/server.jar" || -d "$ROOT/worlds" || -d "$ROOT/world" ]]; then
      if [[ "$NONINTERACTIVE" -eq 1 ]]; then
        EXISTING_MODE=1
      else
        printf "Existing Minecraft server detected. Use --existing mode? [y/N]: "
        read -r ans || true
        ans="${ans:-N}"
        case "$(printf '%s' "$ans" | tr '[:upper:]' '[:lower:]')" in
          y|yes) EXISTING_MODE=1 ;;
        esac
      fi
    fi
  fi

  # Defaults for wrapper-config patching.
  CONTROL_PORT=$((HTTP_PORT + 1))
  MINECRAFT_LAUNCH_JAR="server.jar"

  local cfg_path="$ROOT/wrapper-config.toml"
  local have_cfg=0
  local extracted_http_port extracted_control_port has_identity_id connector_auto_update cfg_auto_start cfg_launch_jar
  if [[ -f "$cfg_path" ]]; then
    have_cfg=1
    read -r extracted_http_port extracted_control_port has_identity_id connector_auto_update cfg_auto_start cfg_launch_jar < <(extract_wrapper_config_fields "$cfg_path")
    if [[ "$extracted_http_port" -gt 0 ]]; then HTTP_PORT="$extracted_http_port"; fi
    if [[ "$extracted_control_port" -gt 0 ]]; then CONTROL_PORT="$extracted_control_port"; fi
  fi

  if [[ "$EXISTING_MODE" -eq 1 ]]; then
    # Existing-server mode: detect jar + keep minecraft.auto-start=false unless opted in.
    MINECRAFT_LAUNCH_JAR="$(detect_existing_launch_jar)"
    if [[ "$MINECRAFT_AUTO_START" -ne 1 ]]; then
      if [[ "$NONINTERACTIVE" -ne 1 ]]; then
        printf "Enable minecraft.auto-start in wrapper-config for this existing server? [y/N]: "
        read -r ans || true
        ans="${ans:-N}"
        case "$(printf '%s' "$ans" | tr '[:upper:]' '[:lower:]')" in
          y|yes) MINECRAFT_AUTO_START=1 ;;
        esac
      fi
    fi
    MINECRAFT_CHOICE="none"
    log "Existing server detected: using launch-jar '$MINECRAFT_LAUNCH_JAR' and minecraft.auto-start=$([[ $MINECRAFT_AUTO_START -eq 1 ]] && echo true || echo false)."
    log "Suggestion: for isolation, consider scan-root separation via a 'vesta-pack/' folder."
  else
    MINECRAFT_CHOICE="${MINECRAFT_CHOICE:-}"
    MINECRAFT_AUTO_START=0
    MINECRAFT_LAUNCH_JAR="server.jar"
  fi

  # Port preflight: probe HTTP/control ports and choose an alternate pair if busy.
  if [[ "$HTTP_PORT" -le 0 ]]; then HTTP_PORT=9090; fi
  if [[ "$CONTROL_PORT" -le 0 ]]; then CONTROL_PORT=$((HTTP_PORT + 1)); fi
  read -r HTTP_PORT CONTROL_PORT < <(choose_free_port_pair "$HTTP_PORT" "$CONTROL_PORT")

  local needs_mutation=0
  if [[ -d "$ROOT/vesta-wrapper" ]]; then
    needs_mutation=1
  fi

  # Detect whether wrapper-config needs meaningful mutation (ports, identity.id, connector, minecraft start).
  if [[ "$have_cfg" -eq 1 ]]; then
    local cfg_mut=0
    if [[ "$HTTP_PORT" -ne "$extracted_http_port" || "$CONTROL_PORT" -ne "$extracted_control_port" ]]; then cfg_mut=1; fi
    if [[ "$has_identity_id" -ne 1 ]]; then cfg_mut=1; fi
    if [[ "$connector_auto_update" -ne 0 ]]; then cfg_mut=1; fi
    if [[ "$cfg_auto_start" -ne "$MINECRAFT_AUTO_START" ]]; then cfg_mut=1; fi
    if [[ -n "$cfg_launch_jar" && "$cfg_launch_jar" != "$MINECRAFT_LAUNCH_JAR" ]]; then cfg_mut=1; fi
    if [[ "$cfg_mut" -eq 1 ]]; then needs_mutation=1; fi
  fi

  if [[ "$needs_mutation" -eq 1 && "$BACKUP" -eq 0 && "$I_UNDERSTAND_RISK" -eq 0 ]]; then
    die "Installer will mutate existing wrapper files/config. Re-run with --backup or --i-understand-risk."
  fi
  if [[ "$needs_mutation" -eq 1 && "$BACKUP" -eq 1 ]]; then
    create_backup
  fi

  scaffold
  install_wrapper
  write_config
  write_start_script

  if [[ "$EXISTING_MODE" -ne 1 ]]; then
    prompt_minecraft
    download_minecraft
  else
    log "Existing server mode: not downloading a new minecraft jar."
  fi

  port_forward_loop
  install_service

  if ensure_wrapper_running_for_verify; then
    log "Verified: wrapper responded on /api/v1/status (instance: $INSTANCE)."
  else
    warn "Verify step failed: wrapper did not respond on /api/v1/status yet."
  fi

  # Upsert instance entry (identity/discovery only; ports are read from wrapper-config at runtime).
  local server_id
  server_id="$(toml_server_identity_id "$ROOT/wrapper-config.toml")"
  registry_upsert "$INSTANCE" "$ROOT/wrapper-config.toml" "$ROOT" "$server_id" "$service_name"

  cat <<EOF

Done.
  Instance:        $INSTANCE
  Start manually:  $ROOT/start-vesta-wrapper.sh
  Devtools:        https://localhost:${HTTP_PORT}/devtools
  Status:          https://localhost:${HTTP_PORT}/api/v1/status
  Next steps:
    vesta-wrapperctl status --instance $INSTANCE
    vesta-wrapperctl logs --instance $INSTANCE
  Docs:            ${REPO_HINT}

The Vesta connector mod/plugin was NOT installed. For non-vanilla servers,
install it later from GitHub Releases / panel one-click when available.
Set connector.auto-update = true only when you intentionally opt in later.
EOF
}

main

