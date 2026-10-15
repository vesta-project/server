#!/usr/bin/env bash
set -euo pipefail

usage() { echo 'Usage: install.sh --setup-url HTTPS_URL --setup-sha256 HEX --bundle-url HTTPS_URL --manifest-sha256 HEX --release-key HEX [--answers PATH] [--yes]' >&2; exit 2; }
setup_url= setup_sha= bundle_url= manifest_sha= release_key= answers= yes=false
while (($#)); do
  case "$1" in
    --setup-url) (($# >= 2)) || usage; setup_url=$2; shift 2;;
    --setup-sha256) (($# >= 2)) || usage; setup_sha=$2; shift 2;;
    --bundle-url) (($# >= 2)) || usage; bundle_url=$2; shift 2;;
    --manifest-sha256) (($# >= 2)) || usage; manifest_sha=$2; shift 2;;
    --release-key) (($# >= 2)) || usage; release_key=$2; shift 2;;
    --answers) (($# >= 2)) || usage; answers=$2; shift 2;;
    --yes) yes=true; shift;;
    *) usage;;
  esac
done
[[ -n $setup_url && -n $bundle_url && -n $setup_sha && -n $manifest_sha && -n $release_key ]] || usage
[[ $setup_url =~ ^https://[^/@\?#]+(/[^\?#]*)?$ && $bundle_url =~ ^https://[^/@\?#]+(/[^\?#]*)?$ ]] || { echo 'Remote URLs must be clean HTTPS URLs without credentials, query, or fragment.' >&2; exit 2; }
[[ $setup_sha =~ ^[[:xdigit:]]{64}$ && $manifest_sha =~ ^[[:xdigit:]]{64}$ && $release_key =~ ^[[:xdigit:]]{64}$ ]] || { echo 'SHA-256 and release key values must be 64 hex characters.' >&2; exit 2; }
command -v curl >/dev/null || { echo 'curl is required.' >&2; exit 2; }
temp_root=${TMPDIR:-/tmp}
work=$(mktemp -d "$temp_root/vesta-setup-bootstrap.XXXXXXXX")
cleanup() {
  case "$work" in "$temp_root"/vesta-setup-bootstrap.*) rm -rf -- "$work";; *) echo 'Refusing unsafe temporary cleanup.' >&2;; esac
}
trap cleanup EXIT HUP INT TERM
setup=$work/vesta-setup
if ! curl --fail --silent --show-error --location --max-time 180 --proto '=https' --proto-redir '=https' --max-filesize 67108864 "$setup_url" | head -c 67108865 > "$setup"; then
  echo 'Failed to download setup binary (or it exceeds 64 MiB).' >&2; exit 1
fi
size=$(wc -c < "$setup" | tr -d '[:space:]')
(( size <= 67108864 )) || { echo 'Setup binary exceeds 64 MiB.' >&2; exit 1; }
actual=$(sha256sum "$setup" | awk '{print $1}')
[[ ${actual,,} == ${setup_sha,,} ]] || { echo 'Setup SHA-256 mismatch.' >&2; exit 1; }
chmod 700 "$setup"
args=(--bundle-url "$bundle_url" --manifest-sha256 "${manifest_sha,,}" --release-key "${release_key,,}")
if [[ -n $answers ]]; then args+=(--answers "$answers"); fi
if [[ $yes == true ]]; then args+=(--yes); fi
if [[ -z $answers && -t 1 && -r /dev/tty ]]; then
  "$setup" "${args[@]}" </dev/tty
else
  "$setup" "${args[@]}"
fi
