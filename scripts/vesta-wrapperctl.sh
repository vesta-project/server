#!/usr/bin/env bash
# Multi-instance control CLI for Vesta wrapper installs.
# Registry: $HOME/.vesta/instances.json (identity only; ports from wrapper-config.toml).
set -euo pipefail

die() { echo "error: $*" >&2; exit 1; }
info() { echo "[vesta-wrapperctl] $*" >&2; }
warn() { echo "[vesta-wrapperctl] warn: $*" >&2; }

usage() {
  cat <<'EOF'
Usage: vesta-wrapperctl.sh <command> [options]

Commands:
  list                              Show registered instances
  discover --roots <dir[,dir...]>   Scan for wrapper-config.toml
                                    (optional --register to upsert finds)
  register --config <path> [--name] Manual registry add/upsert
  unregister --config|--instance    Remove registry entry
  status                            Config ports + GET /api/v1/status
  start | stop | restart            OS service (registry service_name), else start script
  logs [--lines N] [--follow]       Tail .vesta/*.log or console API
  api <METHOD> <path> [body]        Local HTTPS client (curl-like)
  devtools                          Print loopback devtools URL
  uninstall [--delete-files] [--yes]
                                    Stop service, unregister, remove wrapper artifacts
                                    (keep server files unless --delete-files)

Instance selectors (precedence):
  --config <path> > --instance <name> > --http-port <n> > single registered instance

Auth (for logs / protected api):
  --token <bearer>   or env VESTA_TOKEN
  --password <pass>

Other:
  --yes / -y         Non-interactive confirmations
  -h / --help        This help
EOF
}

OPT_CONFIG=""
OPT_INSTANCE=""
OPT_HTTP_PORT=""
OPT_NAME=""
OPT_ROOTS=""
OPT_REGISTER=0
OPT_LINES=100
OPT_FOLLOW=0
OPT_DELETE_FILES=0
OPT_YES=0
OPT_TOKEN="${VESTA_TOKEN:-}"
OPT_PASSWORD=""
POSITIONAL=()

COMMAND="${1:-}"
if [[ -z "$COMMAND" || "$COMMAND" == "-h" || "$COMMAND" == "--help" || "$COMMAND" == "help" ]]; then
  usage
  exit 0
fi
shift || true

while [[ $# -gt 0 ]]; do
  case "$1" in
    -h|--help) usage; exit 0 ;;
    --config) OPT_CONFIG="${2:-}"; shift 2 || die "--config requires a path" ;;
    --instance) OPT_INSTANCE="${2:-}"; shift 2 || die "--instance requires a name" ;;
    --http-port) OPT_HTTP_PORT="${2:-}"; shift 2 || die "--http-port requires a number" ;;
    --name) OPT_NAME="${2:-}"; shift 2 || die "--name requires a value" ;;
    --roots) OPT_ROOTS="${2:-}"; shift 2 || die "--roots requires a value" ;;
    --register) OPT_REGISTER=1; shift ;;
    --lines) OPT_LINES="${2:-}"; shift 2 || die "--lines requires a number" ;;
    --follow) OPT_FOLLOW=1; shift ;;
    --delete-files) OPT_DELETE_FILES=1; shift ;;
    --yes|-y) OPT_YES=1; shift ;;
    --token) OPT_TOKEN="${2:-}"; shift 2 || die "--token requires a value" ;;
    --password) OPT_PASSWORD="${2:-}"; shift 2 || die "--password requires a value" ;;
    --) shift; POSITIONAL+=("$@"); break ;;
    *) POSITIONAL+=("$1"); shift ;;
  esac
done

need_cmd() { command -v "$1" >/dev/null 2>&1 || die "required command not found: $1"; }

registry_path() { printf '%s/.vesta/instances.json' "$HOME"; }

slugify() {
  printf '%s' "$1" | tr '[:upper:]' '[:lower:]' \
    | sed -E 's/[^a-z0-9]+/-/g; s/^-+//; s/-+$//; s/--+/-/g'
  echo
}

abs_path() {
  local p="$1"
  if command -v realpath >/dev/null 2>&1; then
    realpath -m "$p" 2>/dev/null || python3 -c 'import os,sys; print(os.path.abspath(sys.argv[1]))' "$p"
  else
    python3 -c 'import os,sys; print(os.path.abspath(sys.argv[1]))' "$p"
  fi
}

read_registry_json() {
  local path
  path="$(registry_path)"
  if [[ ! -f "$path" ]]; then
    printf '%s\n' '{"version":1,"instances":[]}'
    return
  fi
  cat "$path"
}

write_registry_json() {
  local json="$1"
  local path dir tmp
  path="$(registry_path)"
  dir="$(dirname "$path")"
  mkdir -p "$dir"
  tmp="${path}.tmp"
  printf '%s\n' "$json" >"$tmp"
  mv -f "$tmp" "$path"
}

# Prints: http_port|control_port|identity_id|tls_enabled|scheme|public_host|security_mode|security_password
read_wrapper_config() {
  local cfg="$1"
  python3 - "$cfg" <<'PY'
import re, sys
cfg = sys.argv[1]
http_port = None
control_port = None
identity_id = None
tls_enabled = True
scheme = "https"
public_host = "localhost"
public_port = None
security_mode = "open"
security_password = ""
section = None
try:
    lines = open(cfg, "r", encoding="utf-8").read().splitlines()
except FileNotFoundError:
    print("||||true|https|localhost|open|")
    raise SystemExit(0)

for line in lines:
    m = re.match(r'^\s*\[([^\]]+)\]\s*$', line)
    if m:
        section = m.group(1).strip()
        continue
    if line.strip().startswith("#"):
        continue
    if section == "server":
        m = re.match(r'^\s*http-port\s*=\s*(\d+)\s*$', line)
        if m:
            http_port = int(m.group(1)); continue
        m = re.match(r'^\s*tcp-port\s*=\s*(\d+)\s*$', line)
        if m and http_port is None:
            http_port = int(m.group(1)); continue
    elif section == "server.public":
        m = re.match(r'^\s*scheme\s*=\s*"?([^"#]+)"?\s*$', line)
        if m: scheme = m.group(1).strip(); continue
        m = re.match(r'^\s*host\s*=\s*"?([^"#]+)"?\s*$', line)
        if m: public_host = m.group(1).strip(); continue
        m = re.match(r'^\s*port\s*=\s*(\d+)\s*$', line)
        if m: public_port = int(m.group(1)); continue
    elif section == "server.tls":
        m = re.match(r'^\s*enabled\s*=\s*(true|false)\s*$', line)
        if m:
            tls_enabled = (m.group(1) == "true")
            if not tls_enabled:
                scheme = "http"
            continue
    elif section == "server.identity":
        m = re.match(r'^\s*id\s*=\s*"?([^"#]+)"?\s*$', line)
        if m: identity_id = m.group(1).strip(); continue
    elif section == "security":
        m = re.match(r'^\s*control-port\s*=\s*(\d+)\s*$', line)
        if m: control_port = int(m.group(1)); continue
        m = re.match(r'^\s*mode\s*=\s*"?([^"#]+)"?\s*$', line)
        if m: security_mode = m.group(1).strip(); continue
        m = re.match(r'^\s*password\s*=\s*"?([^"#]*)"?\s*$', line)
        if m: security_password = m.group(1); continue

if http_port is None and public_port is not None:
    http_port = public_port
if not tls_enabled:
    scheme = "http"

def n(v):
    return "" if v is None else str(v)

print("|".join([
    n(http_port), n(control_port), n(identity_id),
    "true" if tls_enabled else "false",
    scheme, public_host, security_mode, security_password
]))
PY
}

parse_cfg_field() {
  # usage: parse_cfg_field "$fields" index(0-based)
  local fields="$1" idx="$2"
  python3 -c 'import sys; parts=sys.argv[1].split("|"); i=int(sys.argv[2]); print(parts[i] if i < len(parts) else "")' "$fields" "$idx"
}

base_url_from_cfg() {
  local fields="$1"
  local http_port scheme host
  http_port="$(parse_cfg_field "$fields" 0)"
  scheme="$(parse_cfg_field "$fields" 4)"
  host="$(parse_cfg_field "$fields" 5)"
  [[ -n "$http_port" ]] || die "wrapper-config.toml has no http-port (and no public.port fallback). Refusing to guess."
  if [[ "$host" == "0.0.0.0" || "$host" == "*" || "$host" == "::" ]]; then
    host="localhost"
  fi
  [[ -n "$host" ]] || host="localhost"
  [[ -n "$scheme" ]] || scheme="https"
  printf '%s://%s:%s' "$scheme" "$host" "$http_port"
}

http_request() {
  # http_request METHOD URL [body] [token]
  need_cmd curl
  local method="$1" url="$2" body="${3:-}" token="${4:-}"
  local args=(-k -sS -w "\n%{http_code}" -X "$method" "$url")
  if [[ -n "$token" ]]; then
    args+=(-H "Authorization: Bearer $token")
  fi
  if [[ -n "$body" && "$method" != "GET" && "$method" != "HEAD" ]]; then
    args+=(-H "Content-Type: application/json" -d "$body")
  fi
  local out code
  out="$(curl "${args[@]}")" || die "curl failed: $method $url"
  code="${out##*$'\n'}"
  out="${out%$'\n'*}"
  if [[ "$code" -ge 400 ]]; then
    die "HTTP $code $method $url"$'\n'"$out"
  fi
  printf '%s\n' "$out"
}

ensure_token() {
  local fields="$1" base="$2"
  if [[ -n "$OPT_TOKEN" ]]; then
    printf '%s' "$OPT_TOKEN"
    return
  fi
  local mode password
  mode="$(parse_cfg_field "$fields" 6)"
  password="$OPT_PASSWORD"
  if [[ -z "$password" && "$mode" == "password" ]]; then
    password="$(parse_cfg_field "$fields" 7)"
  fi
  if [[ -n "$password" ]]; then
    local body resp
    body="$(python3 -c 'import json,sys; print(json.dumps({"password":sys.argv[1]}))' "$password")"
    resp="$(http_request POST "$base/api/v1/auth/token" "$body" || true)"
    python3 -c 'import json,sys; d=json.load(sys.stdin); print(d.get("token",""))' <<<"$resp"
    return
  fi
  printf ''
}

registry_upsert() {
  local name="$1" config="$2" root="$3" server_id="$4" service_name="$5"
  need_cmd python3
  local path
  path="$(registry_path)"
  python3 - "$path" "$name" "$config" "$root" "$server_id" "$service_name" <<'PY'
import json, os, sys, datetime
path, name, config, root, server_id, service_name = sys.argv[1:7]
config = os.path.abspath(config)
root = os.path.abspath(root)
if os.path.isfile(path):
    with open(path, "r", encoding="utf-8") as f:
        data = json.load(f)
else:
    data = {"version": 1, "instances": []}
data.setdefault("version", 1)
instances = data.get("instances") or []
if not isinstance(instances, list):
    instances = []
now = datetime.datetime.utcnow().strftime("%Y-%m-%dT%H:%M:%SZ")
found = None
for inst in instances:
    if os.path.abspath(inst.get("config","")) == config:
        found = inst
        break
if found is not None:
    found["name"] = name
    found["root"] = root
    found["service_name"] = service_name
    if server_id:
        found["server_id"] = server_id
    found.pop("http_port", None)
    found.pop("control_port", None)
else:
    entry = {
        "name": name,
        "config": config,
        "root": root,
        "service_name": service_name,
        "registered_at": now,
    }
    if server_id:
        entry["server_id"] = server_id
    instances.append(entry)
data["instances"] = instances
os.makedirs(os.path.dirname(path), exist_ok=True)
tmp = path + ".tmp"
with open(tmp, "w", encoding="utf-8") as f:
    json.dump(data, f, indent=2)
    f.write("\n")
os.replace(tmp, path)
PY
}

list_instances_json() {
  need_cmd python3
  python3 - "$(registry_path)" <<'PY'
import json, os, sys
path = sys.argv[1]
if not os.path.isfile(path):
    print("[]")
    raise SystemExit(0)
with open(path, "r", encoding="utf-8") as f:
    data = json.load(f)
print(json.dumps(data.get("instances") or []))
PY
}

resolve_target() {
  # Sets TARGET_NAME TARGET_CONFIG TARGET_ROOT TARGET_SERVICE
  need_cmd python3
  local json
  json="$(list_instances_json)"

  if [[ -n "$OPT_CONFIG" ]]; then
    local abs root slug
    abs="$(abs_path "$OPT_CONFIG")"
    [[ -f "$abs" ]] || die "Config not found: $abs"
    # Prefer registry match
    local hit
    hit="$(python3 -c 'import json,os,sys
cfg=os.path.abspath(sys.argv[1])
for i in json.loads(sys.argv[2]):
  if os.path.abspath(i.get("config",""))==cfg:
    print(json.dumps(i)); break
' "$abs" "$json" || true)"
    if [[ -n "$hit" ]]; then
      TARGET_NAME="$(python3 -c 'import json,sys; print(json.load(sys.stdin)["name"])' <<<"$hit")"
      TARGET_CONFIG="$(python3 -c 'import json,sys; print(json.load(sys.stdin)["config"])' <<<"$hit")"
      TARGET_ROOT="$(python3 -c 'import json,sys; print(json.load(sys.stdin)["root"])' <<<"$hit")"
      TARGET_SERVICE="$(python3 -c 'import json,sys; print(json.load(sys.stdin).get("service_name",""))' <<<"$hit")"
      return
    fi
    root="$(dirname "$abs")"
    if [[ -n "$OPT_NAME" ]]; then slug="$(slugify "$OPT_NAME")"
    elif [[ -n "$OPT_INSTANCE" ]]; then slug="$(slugify "$OPT_INSTANCE")"
    else slug="$(slugify "$(basename "$root")")"
    fi
    slug="$(printf '%s' "$slug" | tr -d '\n')"
    TARGET_NAME="$slug"
    TARGET_CONFIG="$abs"
    TARGET_ROOT="$root"
    TARGET_SERVICE="vesta-wrapper-$slug"
    return
  fi

  if [[ -n "$OPT_INSTANCE" ]]; then
    local hit
    hit="$(python3 -c 'import json,sys
name=sys.argv[1]
for i in json.loads(sys.argv[2]):
  if i.get("name")==name:
    print(json.dumps(i)); break
' "$OPT_INSTANCE" "$json" || true)"
    [[ -n "$hit" ]] || die "No registered instance named '$OPT_INSTANCE'. Run: vesta-wrapperctl list"
    TARGET_NAME="$(python3 -c 'import json,sys; print(json.load(sys.stdin)["name"])' <<<"$hit")"
    TARGET_CONFIG="$(python3 -c 'import json,sys; print(json.load(sys.stdin)["config"])' <<<"$hit")"
    TARGET_ROOT="$(python3 -c 'import json,sys; print(json.load(sys.stdin)["root"])' <<<"$hit")"
    TARGET_SERVICE="$(python3 -c 'import json,sys; print(json.load(sys.stdin).get("service_name",""))' <<<"$hit")"
    return
  fi

  if [[ -n "$OPT_HTTP_PORT" ]]; then
    # Match by reading each config's http-port
    local matches=()
    while IFS= read -r line; do
      [[ -n "$line" ]] && matches+=("$line")
    done < <(python3 - "$OPT_HTTP_PORT" "$json" <<'PY'
import json, os, re, sys
want = int(sys.argv[1])
instances = json.loads(sys.argv[2])

def http_port(cfg):
    section=None
    http=None
    public=None
    try:
        lines=open(cfg,"r",encoding="utf-8").read().splitlines()
    except FileNotFoundError:
        return None
    for line in lines:
        m=re.match(r'^\s*\[([^\]]+)\]\s*$', line)
        if m:
            section=m.group(1).strip(); continue
        if line.strip().startswith("#"): continue
        if section=="server":
            m=re.match(r'^\s*http-port\s*=\s*(\d+)\s*$', line)
            if m: http=int(m.group(1))
        elif section=="server.public":
            m=re.match(r'^\s*port\s*=\s*(\d+)\s*$', line)
            if m: public=int(m.group(1))
    return http if http is not None else public

hits=[]
for i in instances:
    cfg=i.get("config")
    if not cfg: continue
    p=http_port(cfg)
    if p==want:
        hits.append(i)
if len(hits)==1:
    print(json.dumps(hits[0]))
elif len(hits)>1:
    print("MULTI:"+ ",".join(h.get("name","?") for h in hits))
else:
    print("")
PY
)
    local hit="${matches[0]:-}"
    if [[ -z "$hit" ]]; then
      die "No registered instance with http-port=$OPT_HTTP_PORT in its wrapper-config.toml. Run: vesta-wrapperctl list"
    fi
    if [[ "$hit" == MULTI:* ]]; then
      die "Multiple instances share http-port=$OPT_HTTP_PORT: ${hit#MULTI:}. Use --config or --instance."
    fi
    TARGET_NAME="$(python3 -c 'import json,sys; print(json.load(sys.stdin)["name"])' <<<"$hit")"
    TARGET_CONFIG="$(python3 -c 'import json,sys; print(json.load(sys.stdin)["config"])' <<<"$hit")"
    TARGET_ROOT="$(python3 -c 'import json,sys; print(json.load(sys.stdin)["root"])' <<<"$hit")"
    TARGET_SERVICE="$(python3 -c 'import json,sys; print(json.load(sys.stdin).get("service_name",""))' <<<"$hit")"
    return
  fi

  local count
  count="$(python3 -c 'import json,sys; print(len(json.loads(sys.argv[1])))' "$json")"
  if [[ "$count" -eq 1 ]]; then
    local hit
    hit="$(python3 -c 'import json,sys; print(json.dumps(json.loads(sys.argv[1])[0]))' "$json")"
    TARGET_NAME="$(python3 -c 'import json,sys; print(json.load(sys.stdin)["name"])' <<<"$hit")"
    TARGET_CONFIG="$(python3 -c 'import json,sys; print(json.load(sys.stdin)["config"])' <<<"$hit")"
    TARGET_ROOT="$(python3 -c 'import json,sys; print(json.load(sys.stdin)["root"])' <<<"$hit")"
    TARGET_SERVICE="$(python3 -c 'import json,sys; print(json.load(sys.stdin).get("service_name",""))' <<<"$hit")"
    return
  fi
  if [[ "$count" -eq 0 ]]; then
    die "No registered instances. Use --config <path>, register one, or run: vesta-wrapperctl list"
  fi
  die "Multiple registered instances; specify --config, --instance, or --http-port. Hint: vesta-wrapperctl list"
}

# --- OS service helpers (use registry service_name as registered) -----------

service_start() {
  local svc="$1"
  [[ -n "$svc" ]] || return 1
  case "$(uname -s)" in
    Linux)
      if systemctl list-unit-files "${svc}.service" >/dev/null 2>&1 || [[ -f "/etc/systemd/system/${svc}.service" ]]; then
        info "Starting system systemd unit ${svc}.service"
        systemctl start "${svc}.service"
        return 0
      fi
      if [[ -f "$HOME/.config/systemd/user/${svc}.service" ]]; then
        info "Starting user systemd unit ${svc}.service"
        systemctl --user start "${svc}.service"
        return 0
      fi
      return 1
      ;;
    Darwin)
      # service_name is launchd label (com.vesta.wrapper.<instance>)
      local domain="gui/$(id -u)"
      if launchctl print "${domain}/${svc}" >/dev/null 2>&1; then
        info "Starting launchd job ${svc}"
        launchctl kickstart -k "${domain}/${svc}" 2>/dev/null \
          || launchctl start "$svc" 2>/dev/null \
          || true
        return 0
      fi
      return 1
      ;;
    *) return 1 ;;
  esac
}

service_stop() {
  local svc="$1"
  [[ -n "$svc" ]] || return 1
  case "$(uname -s)" in
    Linux)
      if [[ -f "/etc/systemd/system/${svc}.service" ]]; then
        info "Stopping system systemd unit ${svc}.service"
        systemctl stop "${svc}.service" || true
        return 0
      fi
      if [[ -f "$HOME/.config/systemd/user/${svc}.service" ]]; then
        info "Stopping user systemd unit ${svc}.service"
        systemctl --user stop "${svc}.service" || true
        return 0
      fi
      return 1
      ;;
    Darwin)
      local domain="gui/$(id -u)"
      if launchctl print "${domain}/${svc}" >/dev/null 2>&1; then
        info "Stopping launchd job ${svc}"
        launchctl bootout "${domain}/${svc}" 2>/dev/null || launchctl stop "$svc" 2>/dev/null || true
        return 0
      fi
      return 1
      ;;
    *) return 1 ;;
  esac
}

service_remove() {
  local svc="$1"
  [[ -n "$svc" ]] || return 1
  case "$(uname -s)" in
    Linux)
      if [[ -f "/etc/systemd/system/${svc}.service" ]]; then
        systemctl disable --now "${svc}.service" 2>/dev/null || true
        rm -f "/etc/systemd/system/${svc}.service"
        systemctl daemon-reload || true
        info "Removed system unit ${svc}.service"
        return 0
      fi
      if [[ -f "$HOME/.config/systemd/user/${svc}.service" ]]; then
        systemctl --user disable --now "${svc}.service" 2>/dev/null || true
        rm -f "$HOME/.config/systemd/user/${svc}.service"
        systemctl --user daemon-reload || true
        info "Removed user unit ${svc}.service"
        return 0
      fi
      return 1
      ;;
    Darwin)
      local plist domain="gui/$(id -u)"
      for plist in "/Library/LaunchDaemons/${svc}.plist" "$HOME/Library/LaunchAgents/${svc}.plist"; do
        if [[ -f "$plist" ]]; then
          launchctl bootout "$domain/$svc" 2>/dev/null || true
          rm -f "$plist"
          info "Removed launchd plist $plist"
          return 0
        fi
      done
      return 1
      ;;
    *) return 1 ;;
  esac
}

start_manual() {
  local root="$1" config="$2"
  local sh="$root/start-vesta-wrapper.sh"
  local bin="$root/vesta-wrapper/bin/wrapper"
  if [[ -x "$sh" ]]; then
    info "Starting via $sh"
    (cd "$root" && nohup "$sh" >/dev/null 2>&1 &)
    return
  fi
  if [[ -x "$bin" ]]; then
    info "Starting via $bin --config $config"
    (cd "$root" && nohup "$bin" --config "$config" >/dev/null 2>&1 &)
    return
  fi
  die "No OS service '$TARGET_SERVICE' and no start-vesta-wrapper.sh / vesta-wrapper/bin/wrapper under $root"
}

stop_manual() {
  local config="$1"
  # Best-effort: kill java processes whose cmdline mentions this config
  local pids
  pids="$(ps auxww 2>/dev/null | grep -F -- "$config" | grep -v grep | awk '{print $2}' || true)"
  if [[ -z "$pids" ]]; then
    warn "No matching process found for config $config"
    return
  fi
  # shellcheck disable=SC2086
  info "Stopping PIDs: $pids"
  kill $pids 2>/dev/null || true
  sleep 0.5
  # shellcheck disable=SC2086
  kill -9 $pids 2>/dev/null || true
}

wait_status() {
  local base="$1" i
  for i in $(seq 1 20); do
    if curl -k -fsS "$base/api/v1/status" >/dev/null 2>&1; then
      return 0
    fi
    sleep 0.5
  done
  return 1
}

# --- commands ---------------------------------------------------------------

cmd_list() {
  need_cmd python3
  python3 - "$(registry_path)" <<'PY'
import json, os, re, sys
path = sys.argv[1]
if not os.path.isfile(path):
    print(f"No registered instances ({path}).")
    print("Hint: vesta-wrapperctl discover --roots <dir> --register")
    raise SystemExit(0)
with open(path, "r", encoding="utf-8") as f:
    data = json.load(f)
instances = data.get("instances") or []
if not instances:
    print(f"No registered instances ({path}).")
    print("Hint: vesta-wrapperctl discover --roots <dir> --register")
    raise SystemExit(0)

def http_port(cfg):
    section=None
    http=None
    public=None
    try:
        lines=open(cfg,"r",encoding="utf-8").read().splitlines()
    except Exception:
        return "?"
    for line in lines:
        m=re.match(r'^\s*\[([^\]]+)\]\s*$', line)
        if m:
            section=m.group(1).strip(); continue
        if line.strip().startswith("#"): continue
        if section=="server":
            m=re.match(r'^\s*http-port\s*=\s*(\d+)\s*$', line)
            if m: http=m.group(1)
        elif section=="server.public":
            m=re.match(r'^\s*port\s*=\s*(\d+)\s*$', line)
            if m: public=m.group(1)
    return http or public or "?"

print(f"{'NAME':<24} {'HTTP':<8} {'SERVICE':<28} CONFIG")
print(f"{'-'*24} {'-'*8} {'-'*28} {'-'*40}")
for i in instances:
    hp = http_port(i.get("config","")) if i.get("config") else "?"
    print(f"{i.get('name',''):<24} {hp:<8} {i.get('service_name',''):<28} {i.get('config','')}")
PY
}

cmd_discover() {
  [[ -n "$OPT_ROOTS" ]] || die "discover requires --roots <dir[,dir...]>"
  need_cmd python3
  local IFS=','
  # also allow semicolon
  OPT_ROOTS="${OPT_ROOTS//;/,}"
  local root found=0
  for root in $OPT_ROOTS; do
    root="$(echo "$root" | sed 's/^[[:space:]]*//;s/[[:space:]]*$//')"
    [[ -n "$root" ]] || continue
    if [[ ! -d "$root" ]]; then
      warn "Root not found: $root"
      continue
    fi
    while IFS= read -r -d '' cfg; do
      found=1
      local fields hp
      fields="$(read_wrapper_config "$cfg")"
      hp="$(parse_cfg_field "$fields" 0)"
      [[ -n "$hp" ]] || hp="?"
      echo "FOUND  http=${hp}  $cfg"
      if [[ "$OPT_REGISTER" -eq 1 ]]; then
        local server_root name id svc
        server_root="$(dirname "$cfg")"
        if [[ -n "$OPT_NAME" ]]; then name="$(slugify "$OPT_NAME" | tr -d '\n')"
        else name="$(slugify "$(basename "$server_root")" | tr -d '\n')"
        fi
        id="$(parse_cfg_field "$fields" 2)"
        case "$(uname -s)" in
          Darwin) svc="com.vesta.wrapper.${name}" ;;
          *) svc="vesta-wrapper-${name}" ;;
        esac
        registry_upsert "$name" "$cfg" "$server_root" "$id" "$svc"
        info "Registered as '$name' (service_name=$svc)"
      fi
    done < <(find "$root" -type f -name 'wrapper-config.toml' -print0 2>/dev/null)
  done
  if [[ "$found" -eq 0 ]]; then
    echo "No wrapper-config.toml found under: $OPT_ROOTS"
  fi
}

cmd_register() {
  [[ -n "$OPT_CONFIG" ]] || die "register requires --config <path>"
  local abs root name fields id svc
  abs="$(abs_path "$OPT_CONFIG")"
  [[ -f "$abs" ]] || die "Config not found: $abs"
  root="$(dirname "$abs")"
  if [[ -n "$OPT_NAME" ]]; then name="$(slugify "$OPT_NAME" | tr -d '\n')"
  else name="$(slugify "$(basename "$root")" | tr -d '\n')"
  fi
  fields="$(read_wrapper_config "$abs")"
  id="$(parse_cfg_field "$fields" 2)"
  case "$(uname -s)" in
    Darwin) svc="com.vesta.wrapper.${name}" ;;
    *) svc="vesta-wrapper-${name}" ;;
  esac
  registry_upsert "$name" "$abs" "$root" "$id" "$svc"
  info "Registered '$name' -> $abs (service_name=$svc)"
}

cmd_unregister() {
  need_cmd python3
  [[ -n "$OPT_CONFIG" || -n "$OPT_INSTANCE" ]] || die "unregister requires --config or --instance"
  python3 - "$(registry_path)" "${OPT_CONFIG:-}" "${OPT_INSTANCE:-}" <<'PY'
import json, os, sys
path, cfg, name = sys.argv[1:4]
if not os.path.isfile(path):
    print("error: no registry file", file=sys.stderr); raise SystemExit(1)
with open(path, "r", encoding="utf-8") as f:
    data = json.load(f)
instances = data.get("instances") or []
kept=[]
removed=0
want_cfg = os.path.abspath(cfg) if cfg else None
for i in instances:
    drop=False
    if want_cfg and os.path.abspath(i.get("config",""))==want_cfg:
        drop=True
    if name and i.get("name")==name:
        drop=True
    if drop:
        removed += 1
        print(f"[vesta-wrapperctl] Unregistered '{i.get('name')}' ({i.get('config')})", file=sys.stderr)
    else:
        kept.append(i)
if removed==0:
    print("error: No matching registry entry.", file=sys.stderr); raise SystemExit(1)
data["instances"]=kept
tmp=path+".tmp"
with open(tmp,"w",encoding="utf-8") as f:
    json.dump(data,f,indent=2); f.write("\n")
os.replace(tmp, path)
PY
}

cmd_status() {
  resolve_target
  local fields base
  fields="$(read_wrapper_config "$TARGET_CONFIG")"
  echo "instance:     $TARGET_NAME"
  echo "config:       $TARGET_CONFIG"
  echo "root:         $TARGET_ROOT"
  echo "service_name: $TARGET_SERVICE"
  echo "http-port:    $(parse_cfg_field "$fields" 0 || echo '(unset)')"
  echo "control-port: $(parse_cfg_field "$fields" 1 || echo '(unset)')"
  if ! base="$(base_url_from_cfg "$fields")"; then
    die "Cannot resolve base URL from config"
  fi
  echo "base-url:     $base"
  if out="$(curl -k -fsS "$base/api/v1/status" 2>/dev/null)"; then
    echo "api-status:   up"
    echo "$out" | python3 -m json.tool 2>/dev/null || echo "$out"
  else
    echo "api-status:   down (or unreachable)"
    exit 2
  fi
}

cmd_start() {
  resolve_target
  if ! service_start "$TARGET_SERVICE"; then
    start_manual "$TARGET_ROOT" "$TARGET_CONFIG"
  fi
  local fields base
  fields="$(read_wrapper_config "$TARGET_CONFIG")"
  base="$(base_url_from_cfg "$fields")"
  if wait_status "$base"; then
    info "Wrapper is up at $base"
  else
    warn "Started, but /api/v1/status not ready yet ($base)"
  fi
}

cmd_stop() {
  resolve_target
  local used=0 fields base token
  if service_stop "$TARGET_SERVICE"; then used=1; fi
  fields="$(read_wrapper_config "$TARGET_CONFIG")"
  if base="$(base_url_from_cfg "$fields" 2>/dev/null)"; then
    token="$(ensure_token "$fields" "$base")"
    curl -k -fsS -X POST "$base/api/v1/server/power" \
      -H "Content-Type: application/json" \
      ${token:+-H "Authorization: Bearer $token"} \
      -d '{"action":"stop"}' >/dev/null 2>&1 && info "Issued Minecraft power stop via API" || true
  fi
  stop_manual "$TARGET_CONFIG"
  info "Stop complete for '$TARGET_NAME'"
}

cmd_restart() {
  cmd_stop
  sleep 1
  cmd_start
}

cmd_logs() {
  resolve_target
  local log=""
  for cand in \
    "$TARGET_ROOT/.vesta/wrapper.out.log" \
    "$TARGET_ROOT/.vesta/wrapper.err.log" \
    "$TARGET_ROOT/.vesta/wrapper.log" \
    "$TARGET_ROOT/logs/latest.log"
  do
    if [[ -f "$cand" ]]; then log="$cand"; break; fi
  done
  if [[ -n "$log" ]]; then
    info "Tailing $log (last $OPT_LINES lines)"
    if [[ "$OPT_FOLLOW" -eq 1 ]]; then
      tail -n "$OPT_LINES" -F "$log"
    else
      tail -n "$OPT_LINES" "$log"
    fi
    return
  fi
  info "No local log file; trying GET /api/v1/console (needs console.read bearer)"
  local fields base token
  fields="$(read_wrapper_config "$TARGET_CONFIG")"
  base="$(base_url_from_cfg "$fields")"
  token="$(ensure_token "$fields" "$base")"
  [[ -n "$token" ]] || die "No log file under $TARGET_ROOT/.vesta and no --token/VESTA_TOKEN (or --password)."
  local resp since=0
  resp="$(http_request GET "$base/api/v1/console?since=0&limit=$OPT_LINES" "" "$token")"
  python3 -c 'import json,sys
d=json.load(sys.stdin)
for line in d.get("lines") or []:
    print(line.get("text") or line.get("line") or json.dumps(line))
print(d.get("cursor") or 0)
' <<<"$resp" | {
    mapfile -t lines
    if [[ ${#lines[@]} -gt 0 ]]; then
      since="${lines[-1]}"
      unset 'lines[-1]'
      printf '%s\n' "${lines[@]}"
    fi
    if [[ "$OPT_FOLLOW" -eq 1 ]]; then
      warn "--follow via API polls every 1s (Ctrl+C to stop)"
      while true; do
        sleep 1
        resp="$(http_request GET "$base/api/v1/console?since=$since&limit=$OPT_LINES" "" "$token")"
        python3 -c 'import json,sys
d=json.load(sys.stdin)
for line in d.get("lines") or []:
    print(line.get("text") or line.get("line") or json.dumps(line))
c=d.get("cursor")
if c is not None: print("CURSOR:"+str(c), file=sys.stderr)
' <<<"$resp" 2> >(while read -r c; do
          case "$c" in CURSOR:*) since="${c#CURSOR:}" ;; esac
        done)
      done
    fi
  }
}

cmd_api() {
  [[ ${#POSITIONAL[@]} -ge 2 ]] || die "api requires: api <METHOD> <path> [body]"
  local method path body=""
  method="$(printf '%s' "${POSITIONAL[0]}" | tr '[:lower:]' '[:upper:]')"
  path="${POSITIONAL[1]}"
  if [[ ${#POSITIONAL[@]} -ge 3 ]]; then
    body="${POSITIONAL[*]:2}"
  fi
  [[ "$path" == /* ]] || path="/$path"
  resolve_target
  local fields base token
  fields="$(read_wrapper_config "$TARGET_CONFIG")"
  base="$(base_url_from_cfg "$fields")"
  token="$(ensure_token "$fields" "$base")"
  info "$method $base$path"
  local out
  out="$(http_request "$method" "$base$path" "$body" "$token")"
  echo "$out" | python3 -m json.tool 2>/dev/null || echo "$out"
}

cmd_devtools() {
  resolve_target
  local fields base
  fields="$(read_wrapper_config "$TARGET_CONFIG")"
  base="$(base_url_from_cfg "$fields")"
  echo "$base/devtools"
  echo "(loopback-only debug UI — not for production ops)"
}

cmd_uninstall() {
  resolve_target
  if [[ "$OPT_DELETE_FILES" -eq 1 && "$OPT_YES" -ne 1 ]]; then
    printf "DELETE entire server root '%s' for instance '%s'? Type YES to confirm: " "$TARGET_ROOT" "$TARGET_NAME"
    read -r ans || true
    [[ "$ans" == "YES" ]] || die "Aborted."
  elif [[ "$OPT_DELETE_FILES" -ne 1 && "$OPT_YES" -ne 1 ]]; then
    printf "Uninstall wrapper for '%s' (keep Minecraft/server files under %s)? [y/N]: " "$TARGET_NAME" "$TARGET_ROOT"
    read -r ans || true
    case "$(printf '%s' "${ans:-}" | tr '[:upper:]' '[:lower:]')" in
      y|yes) ;;
      *) die "Aborted." ;;
    esac
  fi

  info "Stopping '$TARGET_NAME'..."
  service_stop "$TARGET_SERVICE" || true
  stop_manual "$TARGET_CONFIG" || true
  service_remove "$TARGET_SERVICE" || true

  # Remove registry entry
  OPT_CONFIG="$TARGET_CONFIG" OPT_INSTANCE=""
  # inline unregister by config
  python3 - "$(registry_path)" "$TARGET_CONFIG" <<'PY'
import json, os, sys
path, cfg = sys.argv[1:3]
want = os.path.abspath(cfg)
if os.path.isfile(path):
    with open(path,"r",encoding="utf-8") as f:
        data=json.load(f)
    data["instances"]=[i for i in (data.get("instances") or []) if os.path.abspath(i.get("config",""))!=want]
    tmp=path+".tmp"
    with open(tmp,"w",encoding="utf-8") as f:
        json.dump(data,f,indent=2); f.write("\n")
    os.replace(tmp, path)
print("[vesta-wrapperctl] Removed registry entry", file=sys.stderr)
PY

  if [[ "$OPT_DELETE_FILES" -eq 1 ]]; then
    info "Deleting server root $TARGET_ROOT"
    rm -rf "$TARGET_ROOT"
    info "Uninstall complete (files deleted)."
    return
  fi

  for p in \
    "$TARGET_ROOT/vesta-wrapper" \
    "$TARGET_ROOT/start-vesta-wrapper.sh" \
    "$TARGET_ROOT/start-vesta-wrapper.bat" \
    "$TARGET_ROOT/.vesta/wrapper.out.log" \
    "$TARGET_ROOT/.vesta/wrapper.err.log"
  do
    if [[ -e "$p" ]]; then
      info "Removing $p"
      rm -rf "$p"
    fi
  done
  info "Uninstall complete (server files kept; wrapper-config.toml retained)."
  echo "Hint: delete wrapper-config.toml manually if you no longer need it."
}

case "$COMMAND" in
  list) cmd_list ;;
  discover) cmd_discover ;;
  register) cmd_register ;;
  unregister) cmd_unregister ;;
  status) cmd_status ;;
  start) cmd_start ;;
  stop) cmd_stop ;;
  restart) cmd_restart ;;
  logs) cmd_logs ;;
  api) cmd_api ;;
  devtools) cmd_devtools ;;
  uninstall) cmd_uninstall ;;
  *) die "Unknown command '$COMMAND'. Run with --help." ;;
esac
