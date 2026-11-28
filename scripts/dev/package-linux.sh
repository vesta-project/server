#!/usr/bin/env bash
# Run inside Linux/WSL from a Linux filesystem: deliberately unverified development package.
set -euo pipefail
cd "$(dirname "$0")/../.."
output=${1:-$HOME/vesta-linux-dev}
if [[ -e $output ]]; then
 [[ -f $output/DEVELOPMENT.txt || -z $(ls -A -- "$output") ]] || { echo "Refusing to replace $output; it is not a previous development package." >&2; exit 2; }
 rm -rf -- "$output"
fi
mkdir -p "$output"
output=$(realpath "$output")
export CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-$PWD/target}
cargo build --release -p vesta-agent -p vesta-setup --bins
for name in vesta vesta-agent vesta-setup vesta-supervisor; do
 install -m 755 "$CARGO_TARGET_DIR/release/$name" "$output/$name"
done
install -m 755 scripts/bootstrap/install.sh "$output/install.sh"
printf '%s\n' 'UNVERIFIED DEVELOPMENT BUILD. Do not distribute as a signed release.' \
 "Built from $(git rev-parse --short HEAD 2>/dev/null || echo unknown)$(git diff --quiet HEAD 2>/dev/null || echo ' with local changes') on $(date -u +%Y-%m-%dT%H:%MZ)." > "$output/DEVELOPMENT.txt"
tar -C "$(dirname "$output")" -czf "${output}.tar.gz" "$(basename "$output")"
printf 'Development package: %s.tar.gz\nOn the server: tar -xzf %s.tar.gz -C ~ && sudo bash ~/%s/install.sh --from-dir ~/%s\n' \
 "$output" "$(basename "$output")" "$(basename "$output")" "$(basename "$output")"
