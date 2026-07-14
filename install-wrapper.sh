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

cat > "$TARGET/start-vesta-wrapper.sh" <<EOF
#!/usr/bin/env bash
cd "\$(dirname "\$0")"
exec "\$(dirname "\$0")/vesta-wrapper/bin/wrapper" --config "\$(dirname "\$0")/wrapper-config.toml"
EOF
chmod +x "$TARGET/start-vesta-wrapper.sh"

echo ""
echo "Done. Start sync with: $TARGET/start-vesta-wrapper.sh"
echo "Devtools UI (when running): http://localhost:9090/devtools"
