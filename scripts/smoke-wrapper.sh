#!/usr/bin/env bash
# End-to-end smoke for the Vesta wrapper (Phase 3).
# Mirrors scripts/smoke-wrapper.ps1.
set -euo pipefail

SKIP_BUILD=0
SKIP_MINECRAFT=0
KEEP_ALIVE=0
HTTP_PORT=0
TCP_PORT=0
CONTROL_PORT=0
ROOT=""
REPO_ROOT=""
MC_READY_TIMEOUT=180
INSTANCE_NAME="smoke-test"
PROBE_REL="smoke-e2e-probe.txt"

usage() {
  cat <<'EOF'
Usage: smoke-wrapper.sh [options]
  --root DIR              Server root (default: <repo>/demo-server)
  --repo-root DIR         Repo root (default: parent of scripts/)
  --http-port N           Ephemeral HTTP port (default: free)
  --skip-build            Do not run :wrapper:installDist
  --skip-minecraft        Skip console POST / MC start
  --keep-alive            Leave processes up until Enter
  --minecraft-timeout N   Seconds to wait for Paper ready (default 180)
EOF
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --root) ROOT="$2"; shift 2 ;;
    --repo-root) REPO_ROOT="$2"; shift 2 ;;
    --http-port) HTTP_PORT="$2"; shift 2 ;;
    --tcp-port) TCP_PORT="$2"; shift 2 ;;
    --control-port) CONTROL_PORT="$2"; shift 2 ;;
    --skip-build) SKIP_BUILD=1; shift ;;
    --skip-minecraft) SKIP_MINECRAFT=1; shift ;;
    --keep-alive) KEEP_ALIVE=1; shift ;;
    --minecraft-timeout) MC_READY_TIMEOUT="$2"; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    *) echo "unknown arg: $1" >&2; usage; exit 2 ;;
  esac
done

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${REPO_ROOT:-$(cd "$SCRIPT_DIR/.." && pwd)}"
ROOT="${ROOT:-$REPO_ROOT/demo-server}"
ROOT="$(cd "$ROOT" && pwd)"

PASS=0; FAIL=0; SKIP=0
RESULTS=()

record() {
  local status="$1" name="$2" detail="${3:-}"
  RESULTS+=("[$status] $name${detail:+ — $detail}")
  case "$status" in
    PASS) echo "  PASS  $name"; PASS=$((PASS+1)) ;;
    FAIL) echo "  FAIL  $name — $detail"; FAIL=$((FAIL+1)) ;;
    SKIP) echo "  SKIP  $name — $detail"; SKIP=$((SKIP+1)) ;;
  esac
}

free_port() {
  python3 - <<'PY'
import socket
s=socket.socket(); s.bind(("127.0.0.1",0)); print(s.getsockname()[1]); s.close()
PY
}

[[ "$HTTP_PORT" -eq 0 ]] && HTTP_PORT="$(free_port)"
[[ "$TCP_PORT" -eq 0 ]] && TCP_PORT="$(free_port)"
while [[ "$TCP_PORT" -eq "$HTTP_PORT" ]]; do TCP_PORT="$(free_port)"; done
[[ "$CONTROL_PORT" -eq 0 ]] && CONTROL_PORT="$(free_port)"
while [[ "$CONTROL_PORT" -eq "$HTTP_PORT" || "$CONTROL_PORT" -eq "$TCP_PORT" ]]; do
  CONTROL_PORT="$(free_port)"
done

SMOKE_DIR="$REPO_ROOT/build/smoke-e2e"
mkdir -p "$SMOKE_DIR"
CONFIG="$SMOKE_DIR/wrapper-config.smoke.toml"
LOG_OUT="$SMOKE_DIR/wrapper.out.log"
LOG_ERR="$SMOKE_DIR/wrapper.err.log"
BASE="http://127.0.0.1:$HTTP_PORT"
WRAPPER_PID=""
MC_STARTED=0

SCAN_ROOT="$ROOT/vesta-pack"
if [[ ! -d "$SCAN_ROOT" ]]; then
  SCAN_ROOT="$SMOKE_DIR"
  mkdir -p "$SCAN_ROOT/mods" "$SCAN_ROOT/config"
fi
mkdir -p "$ROOT/.vesta"

LAUNCH_JAR=""
if compgen -G "$ROOT/paper-*.jar" > /dev/null; then
  LAUNCH_JAR="$(basename "$(ls -1 "$ROOT"/paper-*.jar | sort | tail -n1)")"
fi
JAVA_CMD="$(command -v java || echo java)"

cat > "$CONFIG" <<EOF
[server]
tcp-port = $TCP_PORT
http-port = $HTTP_PORT
scan-root = "$SCAN_ROOT"
scan-paths = ["mods", "config"]

[server.public]
scheme = "http"
host = "127.0.0.1"
port = $HTTP_PORT

[server.tls]
enabled = false
auto-generate = false
keystore = ".vesta/tls.p12"
keystore-password = "vesta-generated"

[server.identity]
name = "Vesta Smoke"

[modpack]
name = "Vesta Smoke Pack"
description = "Ephemeral smoke pack"
version = "1.0.0-smoke"
minecraft-version = "1.21.4"
source = "self-hosted"
unresolved-policy = "server"

[modpack.modloader]
type = "fabric"
version = "0.18.4"

[modpack.platform]
project-id = ""
version-id = ""
download-url = ""

[modpack.archive]
path = ""

[modpack.server]
host = "localhost"
port = 25565
description = "smoke"
is-required = false

[minecraft]
auto-start = false
launch-jar = "$LAUNCH_JAR"
working-directory = "$ROOT"
java-command = "$JAVA_CMD"
jvm-args = ["-Xms512M", "-Xmx1G"]
server-args = ["nogui"]

[connector]
auto-update = false

[security]
mode = "open"
password = ""
token-ttl-seconds = 86400
control-port = $CONTROL_PORT
anonymous-level = 4
authenticated-level = 4
password-level = 4
linked-level = 4
whitelist-level = 4

[security.levels]
"0" = ["stats.read"]
"1" = ["stats.read", "pack.fetch"]
"2" = ["stats.read", "pack.fetch", "console.read"]
"3" = ["stats.read", "pack.fetch", "console.read", "files.read"]
"4" = [
  "stats.read",
  "pack.fetch",
  "console.read",
  "files.read",
  "console.execute",
  "files.write",
  "settings.manage",
  "content.manage",
  "server.power",
]

[devtools]
enabled = true
loopback_only = true

[rate_limits]
enabled = true
http_requests_per_minute = 120
websocket_messages_per_second = 30
auth_attempts_per_minute = 20
EOF

api() {
  local method="$1" path="$2" body="${3:-}"
  if [[ -n "$body" ]]; then
    curl -sS -w "\n%{http_code}" -X "$method" "$BASE$path" \
      -H "Content-Type: application/json" -d "$body"
  else
    curl -sS -w "\n%{http_code}" -X "$method" "$BASE$path"
  fi
}

api_raw() {
  local method="$1" path="$2" file="$3"
  curl -sS -w "\n%{http_code}" -X "$method" "$BASE$path" \
    -H "Content-Type: application/octet-stream" --data-binary @"$file"
}

split_http() {
  # stdin: body\ncode -> sets BODY and CODE
  BODY="$(cat)"
  CODE="$(printf '%s' "$BODY" | tail -n1)"
  BODY="$(printf '%s' "$BODY" | sed '$d')"
}

cleanup() {
  if [[ "$MC_STARTED" -eq 1 ]]; then
    echo "[smoke] stopping Minecraft"
    api POST /api/v1/server/power '{"action":"stop"}' >/dev/null 2>&1 || true
    sleep 2
    api POST /api/v1/server/power '{"action":"kill"}' >/dev/null 2>&1 || true
  fi
  if [[ -n "$WRAPPER_PID" ]] && kill -0 "$WRAPPER_PID" 2>/dev/null; then
    echo "[smoke] stopping wrapper pid $WRAPPER_PID"
    kill "$WRAPPER_PID" 2>/dev/null || true
    sleep 1
    kill -9 "$WRAPPER_PID" 2>/dev/null || true
    # child java from start script
    pkill -f "wrapper-config.smoke.toml" 2>/dev/null || true
  fi
  if [[ -x "$REPO_ROOT/scripts/vesta-wrapperctl.sh" ]]; then
    "$REPO_ROOT/scripts/vesta-wrapperctl.sh" unregister --config "$CONFIG" >/dev/null 2>&1 || true
  fi
}
trap cleanup EXIT

echo "[smoke] Repo: $REPO_ROOT"
echo "[smoke] Root: $ROOT"
echo "[smoke] Config: $CONFIG"
echo "[smoke] HTTP :$HTTP_PORT  TCP :$TCP_PORT  control :$CONTROL_PORT"
echo "[smoke] Launch jar: ${LAUNCH_JAR:-'(none)'}"

DEFAULTS="$REPO_ROOT/scripts/defaults/wrapper-config.toml"
if grep -q '^\[rate_limits\]' "$DEFAULTS" && grep -q '^\[rate_limits\]' "$CONFIG"; then
  record PASS rate_limits_config "present in defaults + smoke config"
else
  record FAIL rate_limits_config "[rate_limits] missing"
fi

if [[ "$SKIP_BUILD" -eq 0 ]]; then
  echo "[smoke] Building :wrapper:installDist"
  (cd "$REPO_ROOT" && ./gradlew :wrapper:installDist --quiet)
fi

WRAPPER_BIN="$REPO_ROOT/wrapper/build/install/wrapper/bin/wrapper"
[[ -x "$WRAPPER_BIN" ]] || { echo "missing $WRAPPER_BIN"; exit 1; }

: >"$LOG_OUT"; : >"$LOG_ERR"
(cd "$ROOT" && "$WRAPPER_BIN" --config "$CONFIG" >"$LOG_OUT" 2>"$LOG_ERR") &
WRAPPER_PID=$!

ready=0
for _ in $(seq 1 120); do
  if curl -sS -o /dev/null -w "%{http_code}" "$BASE/api/v1/status" | grep -q 200; then
    ready=1; break
  fi
  sleep 0.4
done
if [[ "$ready" -eq 1 ]]; then record PASS wrapper_ready; else
  record FAIL wrapper_ready "see $LOG_OUT / $LOG_ERR"; exit 1
fi

split_http < <(api GET /api/v1/status)
if [[ "$CODE" == "200" ]] && echo "$BODY" | grep -q security_mode; then
  record PASS status
else
  record FAIL status "HTTP $CODE $BODY"
fi

split_http < <(api POST /api/v1/sync/check '{"current_version":"0.0.0-smoke","installed_hashes":{}}')
if [[ "$CODE" == "200" ]]; then record PASS sync_check; else record FAIL sync_check "HTTP $CODE $BODY"; fi

split_http < <(api GET /api/v1/server/descriptor)
missing=""
for cap in server_descriptor modpack_sync minecraft_challenge realtime_ticket console filesystem \
           server_power content_manage admin_security_loopback sync_check players; do
  echo "$BODY" | grep -q "\"$cap\"" || missing="$missing $cap"
done
if [[ "$CODE" == "200" && -z "$missing" ]] && echo "$BODY" | grep -q '"endpoints"'; then
  record PASS descriptor_capabilities
else
  record FAIL descriptor_capabilities "missing:$missing"
fi

split_http < <(api GET '/api/v1/fs?path=')
if [[ "$CODE" == "200" ]] && echo "$BODY" | grep -q '"entries"'; then
  record PASS fs_list
else
  record FAIL fs_list "HTTP $CODE"
fi

PROBE_FILE="$SMOKE_DIR/probe-body.txt"
printf 'smoke-probe-%s' "$(date -Iseconds)" >"$PROBE_FILE"
split_http < <(api_raw PUT "/api/v1/fs/$PROBE_REL" "$PROBE_FILE")
if [[ "$CODE" == "200" ]] && echo "$BODY" | grep -q '"ok"'; then
  record PASS fs_write
else
  record FAIL fs_write "HTTP $CODE $BODY"
fi

GOT="$(mktemp)"
CODE="$(curl -sS -o "$GOT" -w "%{http_code}" "$BASE/api/v1/fs/$PROBE_REL")"
if [[ "$CODE" == "200" ]] && cmp -s "$GOT" "$PROBE_FILE"; then
  record PASS fs_read "streamed body matched"
else
  record FAIL fs_read "HTTP $CODE"
fi
rm -f "$GOT"

split_http < <(api DELETE "/api/v1/fs/$PROBE_REL")
if [[ "$CODE" == "200" ]] && echo "$BODY" | grep -q '"deleted"'; then
  record PASS fs_delete
else
  record FAIL fs_delete "HTTP $CODE $BODY"
fi

split_http < <(api GET /api/v1/players)
if [[ "$CODE" == "200" ]] && echo "$BODY" | grep -q '"players"'; then
  record PASS players
else
  record FAIL players "HTTP $CODE $BODY"
fi

if [[ -n "$LAUNCH_JAR" ]]; then
  split_http < <(api GET '/api/v1/console?since=0&limit=50')
  if [[ "$CODE" == "200" ]] && echo "$BODY" | grep -q '"cursor"'; then
    record PASS console_get
  else
    record FAIL console_get "HTTP $CODE $BODY"
  fi
else
  record SKIP console_get "no paper jar"
fi

if [[ "$SKIP_MINECRAFT" -eq 1 || -z "$LAUNCH_JAR" ]]; then
  reason="--skip-minecraft"
  [[ -z "$LAUNCH_JAR" ]] && reason="no launch jar"
  record SKIP console_post "$reason"
  record SKIP console_cursor_progress "depends on console_post"
  record SKIP console_realtime "depends on console_post"
else
  echo "[smoke] Starting Minecraft (timeout ${MC_READY_TIMEOUT}s)"
  split_http < <(api POST /api/v1/server/power '{"action":"start"}')
  if [[ "$CODE" != "200" ]]; then
    record FAIL console_post "power start HTTP $CODE $BODY"
    record SKIP console_cursor_progress "MC failed"
    record SKIP console_realtime "MC failed"
  else
    MC_STARTED=1
    ready=0
    since=0
    deadline=$((SECONDS + MC_READY_TIMEOUT))
    while (( SECONDS < deadline )); do
      split_http < <(api GET "/api/v1/console?since=$since&limit=200")
      since="$(echo "$BODY" | python3 -c 'import sys,json; d=json.load(sys.stdin); print(d.get("cursor",0))' 2>/dev/null || echo "$since")"
      if echo "$BODY" | grep -Eqi 'Done \(|For help, type|Timings Reset'; then
        ready=1; break
      fi
      sleep 2
    done
    if [[ "$ready" -ne 1 ]]; then
      record FAIL console_post "MC not ready within ${MC_READY_TIMEOUT}s"
      record SKIP console_cursor_progress "MC not ready"
      record SKIP console_realtime "MC not ready"
    else
      split_http < <(api GET '/api/v1/console?since=0&limit=1')
      cursor_before="$(echo "$BODY" | python3 -c 'import sys,json; print(json.load(sys.stdin).get("cursor",0))')"
      split_http < <(api POST /api/v1/console '{"command":"list"}')
      if [[ "$CODE" == "200" ]] && echo "$BODY" | grep -q '"ok"'; then
        record PASS console_post "command=list accepted"
      else
        record FAIL console_post "HTTP $CODE $BODY"
      fi
      sleep 2
      split_http < <(api GET "/api/v1/console?since=$cursor_before&limit=100")
      cursor_after="$(echo "$BODY" | python3 -c 'import sys,json; print(json.load(sys.stdin).get("cursor",0))')"
      lines="$(echo "$BODY" | python3 -c 'import sys,json; print(len(json.load(sys.stdin).get("lines",[])))')"
      if [[ "$cursor_after" -gt "$cursor_before" && "$lines" -ge 1 ]]; then
        record PASS console_cursor_progress "cursor $cursor_before -> $cursor_after (+$lines lines)"
      else
        record FAIL console_cursor_progress "cursor $cursor_before -> $cursor_after lines=$lines"
      fi
      record SKIP console_realtime "WS console_line requires linked bearer ticket (open smoke uses REST cursor only)"
    fi
  fi
fi

if [[ -x "$REPO_ROOT/scripts/vesta-wrapperctl.sh" ]]; then
  "$REPO_ROOT/scripts/vesta-wrapperctl.sh" register --config "$CONFIG" --name "$INSTANCE_NAME" || true
  if "$REPO_ROOT/scripts/vesta-wrapperctl.sh" status --instance "$INSTANCE_NAME"; then
    record PASS wrapperctl_status
  else
    record FAIL wrapperctl_status "ctl status failed"
  fi
else
  record SKIP wrapperctl_status "vesta-wrapperctl.sh missing"
fi

if [[ "$KEEP_ALIVE" -eq 1 ]]; then
  echo "[smoke] KeepAlive — press Enter to tear down"
  read -r _
fi

echo
echo "=== Smoke summary ==="
printf '%s\n' "${RESULTS[@]}"
echo "Totals: $PASS pass / $FAIL fail / $SKIP skip"
[[ "$FAIL" -eq 0 ]]
