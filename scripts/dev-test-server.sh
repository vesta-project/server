#!/usr/bin/env bash
# Linux development fixture. Dependencies: Rust/Cargo, curl, Python 3; Java for Minecraft.
set -euo pipefail
repo=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
root="${TMPDIR:-/tmp}/vesta-dev-server"
port=8443 game_port=25565 version=latest java_bin="" loader=vanilla loader_version=""
accept_eula=0 inert=0 background=0 reset=0 force=0 build_profile=debug
build_args=()
while (($#)); do
    case "$1" in
        --root|--port|--game-port|--version|--java|--loader|--loader-version)
            [[ $# -ge 2 && -n $2 ]] || { echo "Missing value for $1" >&2; exit 2; }
            case "$1" in
                --root) root=$2;; --port) port=$2;; --game-port) game_port=$2;;
                --version) version=$2;; --java) java_bin=$2;; --loader) loader=$2;;
                --loader-version) loader_version=$2;;
            esac
            shift 2;;
        --accept-eula) accept_eula=1; shift;; --inert) inert=1; shift;;
        --background) background=1; shift;; --reset) reset=1; shift;;
        --force) force=1; shift;; --release) build_profile=release; build_args=(--release); shift;;
        --help|-h)
            echo "Usage: $0 [--accept-eula | --inert] [--root PATH] [--port 8443] [--game-port 25565] [--version latest] [--java PATH] [--loader vanilla|fabric] [--loader-version VERSION] [--release] [--background]"
            echo "       $0 [--root PATH] --reset [--force]"; exit 0;;
        *) echo "Unknown option: $1" >&2; exit 2;;
    esac
done
if ((!reset && !inert && !accept_eula)); then
    echo 'Read the Minecraft EULA: https://www.minecraft.net/eula'
    echo 'Run again with --accept-eula only if you agree. No installation or acceptance performed.'
    exit 0
fi
command -v python3 >/dev/null || { echo 'Python 3 is required.' >&2; exit 1; }
[[ $port =~ ^[0-9]+$ && $game_port =~ ^[0-9]+$ ]] && ((port>0 && port<65536 && game_port>0 && game_port<65536)) || { echo 'Invalid port.' >&2; exit 2; }
[[ $loader == vanilla || $loader == fabric ]] || { echo 'Loader must be vanilla or fabric.' >&2; exit 2; }
# Validate the original lexical path before resolving; reject every symlink ancestor.
root=$(python3 - "$root" "$repo" <<'PY'
import pathlib, sys
p = pathlib.Path(sys.argv[1]).absolute()
for ancestor in (p, *p.parents):
    if ancestor.is_symlink():
        raise SystemExit(f'Scratch path contains a symlink: {ancestor}')
p = p.resolve()
repo = pathlib.Path(sys.argv[2]).resolve()
if p == pathlib.Path('/') or p == repo or p in repo.parents:
    raise SystemExit('Root must be a dedicated scratch directory outside repository ancestors.')
print(p)
PY
)
marker="$root/.vesta-dev-server"
if ((reset)); then
    if [[ -e $root ]]; then
        [[ -f $marker && $(cat -- "$marker") == vesta-dev-server-v1 ]] || { echo 'Reset refused: unmarked scratch root.' >&2; exit 1; }
        if [[ -f $root/agent.pid ]] && kill -0 "$(cat -- "$root/agent.pid")" 2>/dev/null; then
            echo 'Stop the dev server before resetting.' >&2; exit 1
        fi
        [[ -z $(find "$root" -type l -print -quit) ]] || { echo 'Reset refused: linked child.' >&2; exit 1; }
        if ((!force)); then
            read -r -p "Delete all disposable data at '$root'? Type DELETE: " answer
            [[ $answer == DELETE ]] || { echo 'Reset cancelled.'; exit 0; }
        fi
        rm -rf -- "$root"
    fi
    echo "Deleted scratch server: $root"; exit 0
fi
if ((!inert)); then
    command -v curl >/dev/null || { echo 'curl is required for latest-release lookup.' >&2; exit 1; }
    if [[ -z $java_bin ]]; then
        if [[ -n ${JAVA_HOME:-} && -x $JAVA_HOME/bin/java ]]; then java_bin="$JAVA_HOME/bin/java"
        else java_bin=$(command -v java || true); fi
    fi
    [[ -n $java_bin && -x $java_bin ]] || { echo 'Java is missing. Install a suitable JDK, set JAVA_HOME, or pass --java /path/to/java.' >&2; exit 1; }
    java_bin=$(realpath -- "$java_bin")
fi
umask 077
if [[ -e $root ]]; then
    [[ -f $marker ]] || { echo 'Root exists without a dev-server marker; choose another --root.' >&2; exit 1; }
else
    mkdir -p -- "$root"; printf 'vesta-dev-server-v1\n' > "$marker"
fi
if [[ -f $root/agent.pid ]] && kill -0 "$(cat -- "$root/agent.pid")" 2>/dev/null; then
    echo 'Dev server is already running. Stop it before restarting.' >&2; exit 1
fi
cd -- "$repo"
cargo build -p vesta-agent --bin vesta-agent "${build_args[@]}"
cargo build -p vesta-setup --example dev-test-support "${build_args[@]}"
target=${CARGO_TARGET_DIR:-"$repo/target"}
target=$(realpath -- "$target")
mkdir -p -- "$root/bin"
agent="$root/bin/vesta-agent"
cp -- "$target/$build_profile/vesta-agent" "$agent"
helper="$target/$build_profile/examples/dev-test-support"
install_root="$root/servers"
private="$root/private"
mkdir -p -- "$install_root"
"$helper" tls "$private"
mode="$loader:$version"
((!inert)) || mode=inert
[[ ! -f $root/mode || $(cat -- "$root/mode") == "$mode" ]] || { echo 'Mode/version changed: use --reset first or a different --root.' >&2; exit 1; }
if [[ ! -f $install_root/names/dev-server.json ]]; then
    if ((inert)); then
        echo 'INERT MODE: using the integration-test child; no Minecraft software, networking or EULA acceptance.'
        cargo test -p vesta-transport --test https --no-run --message-format=json "${build_args[@]}" > "$root/build-inert.jsonl"
        test_exe=$(python3 - "$root/build-inert.jsonl" <<'PY'
import json, sys
paths = [x['executable'] for line in open(sys.argv[1]) if (x:=json.loads(line)).get('reason') == 'compiler-artifact' and x.get('target',{}).get('name') == 'https' and x.get('executable')]
if not paths: raise SystemExit('Cannot find integration-test child executable.')
print(paths[-1])
PY
)
        "$helper" inert "$install_root" "$test_exe"
    else
        resolved_version=$version
        if [[ $version == latest ]]; then
            resolved_version=$(curl -fsSL --connect-timeout 15 --max-time 60 https://piston-meta.mojang.com/mc/game/version_manifest_v2.json | python3 -c 'import json,sys; print(json.load(sys.stdin)["latest"]["release"])')
        fi
        echo "Installing Minecraft $loader $resolved_version with $java_bin"
        echo 'Installer acknowledgments: first-run effects and official legacy SHA-1 integrity metadata.'
        create_args=(create dev-server --install-root "$install_root" --minecraft-version "$resolved_version" --java "$java_bin" --loader "$loader" --bind 127.0.0.1 --port "$game_port" --approve-install --ack-warnings --ack-legacy-checksum)
        [[ -z $loader_version ]] || create_args+=(--loader-version "$loader_version")
        "$agent" "${create_args[@]}"
    fi
    printf '%s\n' "$mode" > "$root/mode"
fi
config=$(python3 - "$install_root" "$private" "$port" "$inert" <<'PY'
import json, pathlib, re, sys
root, private = map(pathlib.Path,sys.argv[1:3])
instance = json.loads((root/'names/dev-server.json').read_text())['instance']
if not re.fullmatch(r'srv_[0-9a-f]{32}',instance): raise SystemExit('Invalid scratch instance record.')
directory = root/'instances'/instance
if sys.argv[4] == '0':
    (directory/'game/eula.txt').write_text('# Accepted explicitly with --accept-eula. https://www.minecraft.net/eula\neula=true\n')
path = directory/'host.toml'
listener = f"[listener]\nmode = 'tls'\nbind = '127.0.0.1'\nport = {sys.argv[3]}\npublic-endpoint = 'https://127.0.0.1:{sys.argv[3]}/'\ncertificate = {json.dumps(str(private/'certificate.pem'))}\nprivate-key = {json.dumps(str(private/'key.pem'))}\nmax-connections = 32\nmax-request-bytes = 1048576\nrequest-timeout-seconds = 30\npeer-requests-per-minute = 6000\npeer-connections-per-minute = 6000\nsession-requests-per-minute = 6000\n"
path.write_text(re.sub(r'(?ms)^\[listener\]\s*.*?(?=^\[|\Z)', lambda _:listener,path.read_text()))
print(path)
PY
)
"$agent" check-config "$config"
"$agent" serve "$config" > "$root/agent.stdout.log" 2> "$root/agent.stderr.log" &
agent_pid=$!
printf '%s\n' "$agent_pid" > "$root/agent.pid"
completed=0
cleanup() {
    if ((!background || !completed)); then
        "$agent" stop dev-server --install-root "$install_root" || true
        kill -INT "$agent_pid" 2>/dev/null || true
        wait "$agent_pid" || true
    fi
}
trap cleanup EXIT
trap 'exit 130' INT TERM
ready=0
for ((attempt=0; attempt<60; attempt++)); do
    kill -0 "$agent_pid" 2>/dev/null || { echo "Agent exited. Read $root/agent.stderr.log" >&2; exit 1; }
    if "$agent" access show dev-server --install-root "$install_root" >/dev/null 2>&1; then ready=1; break; fi
    sleep 0.5
done
((ready)) || { echo "Agent readiness timed out. Read $root/agent.stderr.log" >&2; exit 1; }
echo "=== LAUNCHER PAIRING: https://127.0.0.1:$port/ ==="
"$agent" pair dev-server --install-root "$install_root" --role owner
"$helper" tls "$private"
"$agent" start dev-server --install-root "$install_root"
completed=1
echo "Agent PID: $agent_pid; scratch: $root"
echo "Logs: $root/agent.stdout.log ; $root/agent.stderr.log"
printf 'Stop game: %q stop dev-server --install-root %q\n' "$agent" "$install_root"
echo "Then stop agent: kill -INT $agent_pid"
printf 'Delete after stopping: %q --root %q --reset [--force]\n' "$0" "$root"
if ((background)); then echo 'Background agent is running. Pair using the link above.'; exit 0; fi
echo 'Foreground session: Ctrl+C stops this disposable server.'
tail --pid="$agent_pid" -f "$root/agent.stdout.log" "$root/agent.stderr.log" &
tail_pid=$!
wait "$agent_pid" || true
kill "$tail_pid" 2>/dev/null || true
