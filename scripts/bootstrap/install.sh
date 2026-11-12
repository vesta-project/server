#!/usr/bin/env bash
set -euo pipefail

usage() { echo 'Usage: install.sh (--latest-nightly | --tag RELEASE_TAG | --setup-url HTTPS_URL --setup-sha256 HEX --bundle-url HTTPS_URL --manifest-sha256 HEX --release-key HEX [--asset-prefix TARGET-]) [--answers PATH --yes | --uninstall (--root HOST [--servers-root REGISTRY] | --server NAME --servers-root REGISTRY) [--plan]]' >&2; exit "${1:-2}"; }
setup_url= setup_sha= bundle_url= manifest_sha= release_key= asset_prefix= answers= root= servers_root= server= yes=false uninstall=false plan=false
selection= tag=
while (($#)); do
  case "$1" in
    --help|-h) usage 0;;
    --latest-nightly) [[ -z $selection ]] || usage; selection=nightly; shift;;
    --tag) (($# >= 2)) && [[ -z $selection ]] || usage; selection=tag; tag=$2; shift 2;;
    --setup-url) (($# >= 2)) || usage; setup_url=$2; shift 2;;
    --setup-sha256) (($# >= 2)) || usage; setup_sha=$2; shift 2;;
    --bundle-url) (($# >= 2)) || usage; bundle_url=$2; shift 2;;
    --manifest-sha256) (($# >= 2)) || usage; manifest_sha=$2; shift 2;;
    --release-key) (($# >= 2)) || usage; release_key=$2; shift 2;;
    --asset-prefix) (($# >= 2)) || usage; asset_prefix=$2; shift 2;;
    --answers) (($# >= 2)) || usage; answers=$2; shift 2;;
    --root) (($# >= 2)) || usage; root=$2; shift 2;;
    --servers-root) (($# >= 2)) || usage; servers_root=$2; shift 2;;
    --server) (($# >= 2)) || usage; server=$2; shift 2;;
    --yes) yes=true; shift;;
    --uninstall) uninstall=true; shift;;
    --plan) plan=true; shift;;
    *) usage;;
  esac
done
if [[ -n $selection ]]; then
  [[ -z $setup_url && -z $bundle_url && -z $setup_sha && -z $manifest_sha && -z $release_key && -z $asset_prefix ]] || { echo 'Release selection cannot be mixed with explicit release pins.' >&2; exit 2; }
  [[ $selection != tag || $tag =~ ^[A-Za-z0-9][A-Za-z0-9._+-]{0,63}$ ]] || { echo 'Invalid release tag.' >&2; exit 2; }
  [[ $(uname -s) == Linux ]] || { echo 'Automatic release selection supports Linux only. Use install.ps1 with explicit pins on Windows.' >&2; exit 2; }
  case "$(uname -m)" in x86_64|amd64) target=linux-x86_64;; aarch64|arm64) target=linux-aarch64;; *) echo 'Unsupported Linux architecture.' >&2; exit 2;; esac
  for tool in python3 openssl; do command -v "$tool" >/dev/null || { echo "$tool is required for signed release discovery." >&2; exit 2; }; done
  python3 --version >/dev/null || { echo 'Python 3 is required for release metadata parsing.' >&2; exit 2; }
else
  [[ -n $setup_url && -n $bundle_url && -n $setup_sha && -n $manifest_sha && -n $release_key ]] || usage
  [[ $setup_url =~ ^https://[^/@\?#]+(/[^\?#]*)?$ && $bundle_url =~ ^https://[^/@\?#]+(/[^\?#]*)?$ ]] || { echo 'Remote URLs must be clean HTTPS URLs without credentials, query, or fragment.' >&2; exit 2; }
  [[ $setup_sha =~ ^[[:xdigit:]]{64}$ && $manifest_sha =~ ^[[:xdigit:]]{64}$ && $release_key =~ ^[[:xdigit:]]{64}$ ]] || { echo 'SHA-256 and release key values must be 64 hex characters.' >&2; exit 2; }
fi
if [[ -n $asset_prefix && ! $asset_prefix =~ ^(windows-x86_64|linux-x86_64|linux-aarch64)-$ ]]; then echo 'Asset prefix must be an exact supported target prefix.' >&2; exit 2; fi
if [[ $uninstall == true ]]; then
  [[ -z $server || -z $root ]] || { echo '--server uses --servers-root, not --root.' >&2; exit 2; }
  [[ $yes == false ]] || { echo '--yes is not supported for uninstall.' >&2; exit 2; }
  [[ -z $answers ]] || { echo '--answers is not supported for uninstall.' >&2; exit 2; }
  if [[ -n $server ]]; then [[ -n $servers_root ]] || { echo '--server requires --servers-root.' >&2; exit 2; }
  else [[ -n $root ]] || { echo 'Full Vesta uninstall requires --root.' >&2; exit 2; }; fi
else
  [[ -z $root && -z $servers_root && -z $server && $plan == false ]] || { echo 'Uninstall options require --uninstall.' >&2; exit 2; }
fi
command -v curl >/dev/null || { echo 'curl is required.' >&2; exit 2; }
command -v sha256sum >/dev/null || { echo 'sha256sum is required.' >&2; exit 2; }
umask 077
temp_root=$(cd -P -- "${TMPDIR:-/tmp}" && pwd)
work=$(mktemp -d "$temp_root/vesta-setup-bootstrap.XXXXXXXX")
cleanup() { case "$work" in "$temp_root"/vesta-setup-bootstrap.*) rm -rf -- "$work";; *) echo 'Refusing unsafe temporary cleanup.' >&2;; esac; }
trap cleanup EXIT HUP INT TERM
download() {
  local url=$1 destination=$2 limit=$3 size
  if ! curl --disable --fail --silent --show-error --location --connect-timeout 15 --max-time 180 --max-redirs 5 --proto '=https' --proto-redir '=https' --max-filesize "$limit" "$url" | head -c "$((limit + 1))" > "$destination"; then
    echo 'Download failed or exceeds its size limit.' >&2; return 1
  fi
  size=$(wc -c < "$destination" | tr -d '[:space:]')
  (( size > 0 && size <= limit )) || { echo 'Download is empty or exceeds its size limit.' >&2; return 1; }
}
if [[ -n $selection ]]; then
  # This pin is part of the trusted source script, never read from release assets.
  release_key=84e1fa3eae990d870d671c0fb585a62906f50d91e3382f3cc9f025d904c8dd8a
  api=https://api.github.com/repos/vesta-project/server/releases
  metadata=()
  if [[ $selection == nightly ]]; then
    # Release records include all asset metadata. Bound each ten-record page,
    # while retaining the latest-published selection over at most 100 releases.
    for ((page = 1; page <= 10; page++)); do
      file="$work/releases-$page.json"
      download "$api?per_page=10&page=$page" "$file" 4194304
      count=$(python3 - "$file" <<'PY'
import json, sys
try:
    data = json.load(open(sys.argv[1], encoding='utf-8'))
    if not isinstance(data, list) or len(data) > 10:
        raise ValueError('invalid release page')
    sys.stdout.write(str(len(data)))
except (ValueError, TypeError):
    sys.exit('Invalid GitHub release page.')
PY
)
      metadata+=("$file")
      (( count == 10 )) || break
    done
  else
    download "$api/tags/$tag" "$work/releases.json" 4194304
    metadata+=("$work/releases.json")
  fi
  python3 - "$selection" "$tag" "${metadata[@]}" > "$work/tag" <<'PY'
import datetime, json, re, sys
sys.stdout.reconfigure(newline='\n')
try:
    if sys.argv[1] == 'nightly':
        data = []
        for filename in sys.argv[3:]:
            data.extend(json.load(open(filename, encoding='utf-8')))
        releases = [r for r in data if isinstance(r, dict) and r.get('draft') is False
                    and r.get('prerelease') is True
                    and re.fullmatch(r'nightly-[0-9]{8}-[0-9a-f]{12}', r.get('tag_name', ''))]
        release = max(releases, key=lambda r: datetime.datetime.fromisoformat(r['published_at'].replace('Z', '+00:00')))
    else:
        release = json.load(open(sys.argv[3], encoding='utf-8'))
        if release.get('draft') is not False or release.get('tag_name') != sys.argv[2]:
            raise ValueError('release mismatch')
    print(release['tag_name'])
except (ValueError, TypeError, KeyError, AttributeError):
    sys.exit('No matching published release found in GitHub metadata.')
PY
  tag=$(cat "$work/tag")
  bundle_url="https://github.com/vesta-project/server/releases/download/$tag"
  asset_prefix="$target-"
  download "$bundle_url/${asset_prefix}manifest.json" "$work/manifest.json" 65536
  download "$bundle_url/${asset_prefix}manifest.sig" "$work/manifest.sig" 256
  python3 - "$work" "$release_key" <<'PY'
import base64, pathlib, sys
root = pathlib.Path(sys.argv[1])
try:
    signature = base64.b64decode((root / 'manifest.sig').read_bytes().strip(), validate=True)
    if len(signature) != 64:
        raise ValueError('invalid length')
except ValueError:
    sys.exit('Invalid manifest signature encoding.')
(root / 'signature').write_bytes(signature)
(root / 'key.der').write_bytes(bytes.fromhex('302a300506032b6570032100' + sys.argv[2]))
PY
  openssl pkeyutl -verify -pubin -keyform DER -inkey "$work/key.der" -rawin -in "$work/manifest.json" -sigfile "$work/signature" >/dev/null || { echo 'Manifest signature verification failed. OpenSSL with Ed25519 raw verification support is required.' >&2; exit 1; }
  python3 - "$work/manifest.json" "$tag" "$target" > "$work/pins" <<'PY'
import hashlib, json, re, sys
sys.stdout.reconfigure(newline='\n')
def unique(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError('duplicate key')
        result[key] = value
    return result
try:
    data = open(sys.argv[1], 'rb').read()
    manifest = json.loads(data, object_pairs_hook=unique)
    if (manifest['schema'] != 1 or manifest['version'] != sys.argv[2]
            or manifest['target'] != sys.argv[3]):
        raise ValueError('manifest identity mismatch')
    files = manifest['files']
    names = [f['name'] for f in files]
    if names not in (['vesta-setup', 'vesta-agent', 'vesta-supervisor'],
                     ['vesta-setup', 'vesta-agent', 'vesta-supervisor', 'vesta']):
        raise ValueError('unexpected executable set')
    for f in files:
        if type(f['size']) is not int or f['size'] < 1 or not re.fullmatch('[0-9a-f]{64}', f['sha256']):
            raise ValueError('invalid file identity')
    if files[0]['size'] > 67108864:
        raise ValueError('setup exceeds size limit')
    print(files[0]['sha256'])
    print(files[0]['size'])
    print(hashlib.sha256(data).hexdigest())
except (ValueError, TypeError, KeyError):
    sys.exit('Signed manifest does not match the selected release, platform or executable contract.')
PY
  mapfile -t pins < "$work/pins"
  setup_sha=${pins[0]}; setup_size=${pins[1]}; manifest_sha=${pins[2]}
  setup_url="$bundle_url/${asset_prefix}vesta-setup"
  echo "Installing Vesta $tag ($target)." >&2
fi
setup=$work/vesta-setup
download "$setup_url" "$setup" 67108864
if [[ -n $selection ]]; then [[ $(wc -c < "$setup" | tr -d '[:space:]') == "$setup_size" ]] || { echo 'Setup size mismatch.' >&2; exit 1; }; fi
actual=$(sha256sum "$setup" | awk '{print $1}')
[[ ${actual,,} == ${setup_sha,,} ]] || { echo 'Setup SHA-256 mismatch.' >&2; exit 1; }
chmod 700 "$setup"
args=(--bundle-url "$bundle_url" --manifest-sha256 "${manifest_sha,,}" --release-key "${release_key,,}")
[[ -z $asset_prefix ]] || args+=(--asset-prefix "$asset_prefix")
if [[ $uninstall == true ]]; then
  args=(uninstall "${args[@]}")
  [[ -z $root ]] || args+=(--root "$root")
  [[ -z $servers_root ]] || args+=(--servers-root "$servers_root")
  [[ -z $server ]] || args+=(--server "$server")
  [[ $plan == false ]] || args+=(--plan)
else
  [[ -z $answers ]] || args+=(--answers "$answers")
  [[ $yes == false ]] || args+=(--yes)
fi
if [[ -z $answers && -t 1 && -r /dev/tty ]]; then "$setup" "${args[@]}" </dev/tty; else "$setup" "${args[@]}"; fi
