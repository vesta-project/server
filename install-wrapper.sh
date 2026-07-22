#!/usr/bin/env bash
# Install Vesta sync wrapper into an existing Minecraft server directory.
# Usage: ./install-wrapper.sh /path/to/server --i-have-a-backup

set -euo pipefail
REPO_ROOT="$(cd "$(dirname "$0")" && pwd)"
SERVER_ROOT=""
BACKUP_ACK=0

for arg in "$@"; do
  case "$arg" in
    --i-have-a-backup) BACKUP_ACK=1 ;;
    *) SERVER_ROOT="$arg" ;;
  esac
done

if [[ -z "$SERVER_ROOT" ]]; then
  echo "Usage: $0 /path/to/server --i-have-a-backup"
  exit 1
fi

if [[ "$BACKUP_ACK" -ne 1 ]]; then
  echo "WARNING: Back up mods/, config/, and worlds before continuing."
  echo "Re-run with --i-have-a-backup once you have a backup."
  exit 1
fi

DIST="$REPO_ROOT/wrapper/build/install/wrapper"
if [[ ! -d "$DIST" ]]; then
  echo "Building wrapper..."
  (cd "$REPO_ROOT" && ./gradlew :wrapper:installDist)
fi

TARGET="$(cd "$SERVER_ROOT" && pwd)"
WRAPPER_DIR="$TARGET/vesta-wrapper"
mkdir -p "$WRAPPER_DIR"
cp -a "$DIST/." "$WRAPPER_DIR/"

CONFIG="$TARGET/wrapper-config.toml"
if [[ ! -f "$CONFIG" ]]; then
  cp "$REPO_ROOT/wrapper/src/main/resources/wrapper-config.toml" "$CONFIG"
  echo "Created wrapper-config.toml — edit security.mode before exposing to LAN."
fi

ensure_identity_id() {
  local cfg="$1"
  local new_id
  new_id="$(python3 -c 'import uuid; print(uuid.uuid4())')"

  local section=""
  local header_index=-1
  local has_id=0
  mapfile -t lines < "$cfg"

  for i in "${!lines[@]}"; do
    local line="${lines[$i]}"
    if [[ "$line" =~ ^[[:space:]]*# ]]; then
      continue
    fi
    if [[ "$line" =~ ^[[:space:]]*\\[([^]]+)\\][[:space:]]*$ ]]; then
      section="${BASH_REMATCH[1]}"
      if [[ "$section" == "server.identity" ]]; then
        header_index="$i"
      fi
      continue
    fi
    if [[ "$section" == "server.identity" && "$line" =~ ^[[:space:]]*id[[:space:]]*= ]]; then
      has_id=1
      break
    fi
  done

  if [[ "$has_id" -eq 0 ]]; then
    if [[ "$header_index" -ge 0 ]]; then
      lines=("${lines[@]:0:$((header_index+1))}" "id = \"$new_id\"" "${lines[@]:$((header_index+1))}")
    else
      lines+=("")
      lines+=("[server.identity]")
      lines+=("name = \"Vesta Server\"")
      lines+=("id = \"$new_id\"")
    fi
    printf '%s\n' "${lines[@]}" > "$cfg"
  fi
}

get_http_port() {
  local cfg="$1"
  local p
  p="$(python3 -c 'import re,sys; txt=open(sys.argv[1],encoding="utf-8").read(); m=re.search(r"(?m)^\\s*http-port\\s*=\\s*(\\d+)\\s*$", txt); print(m.group(1) if m else \"9090\")' "$cfg")"
  printf '%s\n' "$p"
}

ensure_identity_id "$CONFIG"
HTTP_PORT="$(get_http_port "$CONFIG")"

cat > "$TARGET/start-vesta-wrapper.sh" <<EOF
#!/usr/bin/env bash
cd "\$(dirname "\$0")"
exec "\$(dirname "\$0")/vesta-wrapper/bin/wrapper" --config "\$(dirname "\$0")/wrapper-config.toml"
EOF
chmod +x "$TARGET/start-vesta-wrapper.sh"

echo ""
echo "Done. Start sync with: $TARGET/start-vesta-wrapper.sh"
echo "Devtools UI (when running): https://localhost:${HTTP_PORT}/devtools"

