#!/usr/bin/env bash
# Package dist/ artifacts locally (wrapper zip + manifest + install scripts).
# Usage: ./scripts/package-dist.sh [output-dir]
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
OUT="${1:-$ROOT/build/dist}"
cd "$ROOT"

./gradlew :wrapper:installDist -q

INSTALL_DIR="$ROOT/wrapper/build/install/wrapper"
STAGE="$OUT/_stage"
rm -rf "$OUT"
mkdir -p "$OUT/wrapper" "$OUT/defaults" "$STAGE"

# Zip the installDist tree so bin/ and lib/ sit at the zip root.
(
  cd "$INSTALL_DIR"
  if command -v zip >/dev/null 2>&1; then
    zip -qr "$OUT/wrapper/latest.zip" .
  else
    python3 - "$INSTALL_DIR" "$OUT/wrapper/latest.zip" <<'PY'
import pathlib, zipfile, sys
src = pathlib.Path(sys.argv[1])
dst = pathlib.Path(sys.argv[2])
with zipfile.ZipFile(dst, "w", zipfile.ZIP_DEFLATED) as zf:
    for path in src.rglob("*"):
        if path.is_file():
            zf.write(path, path.relative_to(src).as_posix())
PY
  fi
)

if command -v sha256sum >/dev/null 2>&1; then
  HASH="$(sha256sum "$OUT/wrapper/latest.zip" | awk '{print $1}')"
else
  HASH="$(shasum -a 256 "$OUT/wrapper/latest.zip" | awk '{print $1}')"
fi

GIT_SHA="$(git rev-parse HEAD 2>/dev/null || echo unknown)"
BUILT_AT="$(date -u +%Y-%m-%dT%H:%M:%SZ)"

cat > "$OUT/manifest.json" <<EOF
{
  "wrapperSha256": "$HASH",
  "builtAt": "$BUILT_AT",
  "gitSha": "$GIT_SHA"
}
EOF

cp "$ROOT/scripts/install.sh" "$OUT/install.sh"
cp "$ROOT/scripts/install.ps1" "$OUT/install.ps1"
cp "$ROOT/scripts/defaults/wrapper-config.toml" "$OUT/defaults/wrapper-config.toml"
chmod +x "$OUT/install.sh"

echo "Packaged dist artifacts in $OUT"
echo "  wrapperSha256=$HASH"
echo "  gitSha=$GIT_SHA"
