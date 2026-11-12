#!/usr/bin/env bash
set -euo pipefail

usage() { echo 'Usage: install.sh [--latest-nightly | --channel stable|nightly | --tag RELEASE_TAG] [--answers PATH --yes]. Without a selection, choose the package inside the installer. Explicit --setup-url/--setup-sha256/--bundle-url/--manifest-sha256/--release-key pins and existing uninstall options remain supported.' >&2; exit "${1:-2}"; }
setup_url= setup_sha= bundle_url= manifest_sha= release_key= asset_prefix= answers= root= servers_root= server= yes=false uninstall=false plan=false
selection= tag= channel= independent=false
while (($#)); do
  case "$1" in
    --help|-h) usage 0;;
    --latest-nightly) [[ -z $selection ]] || usage; selection=nightly; shift;;
    --channel) (($# >= 2)) && [[ -z $selection && ( $2 == stable || $2 == nightly ) ]] || usage; selection=channel; channel=$2; shift 2;;
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
if [[ -z $selection && -z $setup_url && -z $bundle_url && -z $setup_sha && -z $manifest_sha && -z $release_key && -z $asset_prefix ]]; then selection=choose; fi
if [[ -n $selection ]]; then
  [[ -z $setup_url && -z $bundle_url && -z $setup_sha && -z $manifest_sha && -z $release_key && -z $asset_prefix ]] || { echo 'Release selection cannot be mixed with explicit release pins.' >&2; exit 2; }
  [[ $selection != tag || $tag =~ ^[A-Za-z0-9][A-Za-z0-9._+-]{0,63}$ ]] || { echo 'Invalid release tag.' >&2; exit 2; }
  [[ $selection != tag || $tag != installer-* ]] || { echo '--tag selects a server package, not an installer build.' >&2; exit 2; }
  [[ $(uname -s) == Linux ]] || { echo 'Automatic release selection supports Linux only. Use install.ps1 with explicit pins on Windows.' >&2; exit 2; }
  case "$(uname -m)" in x86_64|amd64) target=linux-x86_64;; aarch64|arm64) target=linux-aarch64;; *) echo 'Unsupported Linux architecture.' >&2; exit 2;; esac
  command -v openssl >/dev/null || { echo 'OpenSSL is required for signed release verification.' >&2; exit 2; }
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
  package_tag=$tag
  installer_tag= nightly_tag=
  if [[ -n $selection ]]; then
    # The public feed contains recent published releases without their asset
    # inventories. Only its canonical entry link can select a candidate tag;
    # executable trust comes exclusively from signed metadata below.
    download https://github.com/vesta-project/server/releases.atom "$work/releases.atom" 1048576
    in_entry=false; in_content=false
    release_link='^[[:space:]]*<link[[:space:]][^<>]*href="https://github.com/vesta-project/server/releases/tag/((installer|nightly)-[0-9]{8}-[0-9a-f]{12})"[[:space:]]*/>[[:space:]]*$'
    while IFS= read -r line || [[ -n $line ]]; do
      if [[ $in_content == true ]]; then
        if [[ $line == *'</content>'* ]]; then in_content=false; fi
        continue
      fi
      if [[ $line =~ ^[[:space:]]*\<content([[:space:]]|\>) ]]; then
        in_entry=false
        if [[ $line != *'</content>'* ]]; then in_content=true; fi
      elif [[ $line =~ ^[[:space:]]*\<entry\>[[:space:]]*$ ]]; then in_entry=true
      elif [[ $line =~ ^[[:space:]]*\</entry\>[[:space:]]*$ ]]; then in_entry=false
      elif [[ $in_entry == true && $line == *'rel="alternate"'* && $line =~ $release_link ]]; then
        candidate=${BASH_REMATCH[1]}
        if [[ $candidate == installer-* && -z $installer_tag ]]; then installer_tag=$candidate; fi
        if [[ $candidate == nightly-* && -z $nightly_tag ]]; then nightly_tag=$candidate; fi
        if [[ -n $installer_tag && $uninstall == false ]]; then break; fi
        if [[ -n $nightly_tag && $uninstall == true ]]; then break; fi
      fi
    done < "$work/releases.atom"
    if [[ -n $installer_tag && $uninstall == false ]]; then
      tag=$installer_tag; independent=true
    elif [[ $selection == tag ]]; then tag=$package_tag
    else
      [[ $selection != channel || $channel != stable ]] || { echo 'Stable selection requires a published independent installer.' >&2; exit 1; }
      tag=$nightly_tag
    fi
    [[ -n $tag ]] || { echo 'No published installer or nightly found in the recent GitHub release feed.' >&2; exit 1; }
  fi
  bundle_url="https://github.com/vesta-project/server/releases/download/$tag"
  asset_prefix="$target-"
  if [[ $tag == nightly-20261112-a248a66a6ad3 ]]; then
    # The first immutable nightly predates bootstrap descriptors. These reviewed
    # source pins retain its installation path without trusting unsigned JSON.
    case "$target" in
      linux-x86_64) setup_sha=c9f66b40292a70f32a6584932d10c7a6733866c09a24b3cb7f58304e02848b70; setup_size=18890744; manifest_sha=b68c52742ab8b79836c7435db1530d9b5fa84a9a213081c1e3d084b6246e653c;;
      linux-aarch64) setup_sha=c57a11956f3794f4639b5b7c732d637a4736f380a895141ee97480e30cabdb3b; setup_size=16948776; manifest_sha=0f66f3d20a7e2445efbe01b9c3f2d70dad2e0589d6d184e58698541ba41add2c;;
    esac
  else
    download "$bundle_url/${asset_prefix}bootstrap.txt" "$work/bootstrap.txt" 65536
    download "$bundle_url/${asset_prefix}bootstrap.sig" "$work/bootstrap.sig" 256
    signature=$(tr -d '\r\n' < "$work/bootstrap.sig")
    [[ $signature =~ ^[A-Za-z0-9+/]{86}==$ ]] || { echo 'Invalid bootstrap signature encoding.' >&2; exit 1; }
    printf '%s' "$signature" | openssl base64 -d -A -out "$work/signature"
    der="302a300506032b6570032100$release_key"
    for ((i = 0; i < ${#der}; i += 2)); do printf '%b' "\\x${der:i:2}"; done > "$work/key.der"
    openssl pkeyutl -verify -pubin -keyform DER -inkey "$work/key.der" -rawin -in "$work/bootstrap.txt" -sigfile "$work/signature" >/dev/null || { echo 'Bootstrap signature verification failed. OpenSSL with Ed25519 raw verification support is required.' >&2; exit 1; }
    mapfile -t descriptor < "$work/bootstrap.txt"
    [[ ${#descriptor[@]} == 7 && ${descriptor[1]} == "$tag" && ${descriptor[2]} == "$target" && ${descriptor[3]} == vesta-setup && ${descriptor[4]} =~ ^[0-9a-f]{64}$ && ${descriptor[5]} =~ ^[1-9][0-9]{0,7}$ ]] || { echo 'Signed bootstrap descriptor does not match the selected release, platform or setup identity.' >&2; exit 1; }
    if [[ $independent == true ]]; then
      [[ ${descriptor[0]} == vesta-installer-bootstrap-v1 && ${descriptor[6]} == 1 ]] || { echo 'Signed installer compatibility is invalid.' >&2; exit 1; }
    else
      [[ ${descriptor[0]} == vesta-setup-bootstrap-v1 && ${descriptor[6]} =~ ^[0-9a-f]{64}$ ]] || { echo 'Signed bootstrap descriptor has an invalid purpose or manifest binding.' >&2; exit 1; }
    fi
    printf '%s\n' "${descriptor[@]}" > "$work/bootstrap.canonical"
    [[ $(sha256sum "$work/bootstrap.txt" | awk '{print $1}') == $(sha256sum "$work/bootstrap.canonical" | awk '{print $1}') ]] || { echo 'Noncanonical bootstrap descriptor.' >&2; exit 1; }
    setup_sha=${descriptor[4]}; setup_size=${descriptor[5]}; manifest_sha=${descriptor[6]}
    (( setup_size <= 67108864 )) || { echo 'Setup exceeds size limit.' >&2; exit 1; }
  fi
  setup_url="$bundle_url/${asset_prefix}vesta-setup"
  if [[ $independent == true ]]; then echo "Starting Vesta installer $tag ($target). Choose the server release inside setup." >&2
  else echo "Using release-matched setup for Vesta $tag ($target)." >&2; fi
fi
setup=$work/vesta-setup
download "$setup_url" "$setup" 67108864
if [[ -n $selection ]]; then [[ $(wc -c < "$setup" | tr -d '[:space:]') == "$setup_size" ]] || { echo 'Setup size mismatch.' >&2; exit 1; }; fi
actual=$(sha256sum "$setup" | awk '{print $1}')
[[ ${actual,,} == ${setup_sha,,} ]] || { echo 'Setup SHA-256 mismatch.' >&2; exit 1; }
chmod 700 "$setup"
args=(--bundle-url "$bundle_url" --manifest-sha256 "${manifest_sha,,}" --release-key "${release_key,,}")
[[ -z $asset_prefix ]] || args+=(--asset-prefix "$asset_prefix")
if [[ $independent == true ]]; then
  args=(install)
  case "$selection" in nightly) args+=(--channel nightly);; tag) args+=(--tag "$package_tag");; channel) args+=(--channel "$channel");; esac
fi
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
