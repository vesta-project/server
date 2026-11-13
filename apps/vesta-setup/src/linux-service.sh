#!/usr/bin/env bash
set -euo pipefail
umask 077
fail() { echo "Vesta Server: $*" >&2; exit 1; }
unit() {
cat <<EOF
[Unit]
Description=Vesta Server
After=network-online.target
Wants=network-online.target
[Service]
Type=simple
User=vesta
Group=vesta
WorkingDirectory=$root
ExecStart=/usr/local/lib/vesta/vesta-agent host $root/host.toml
Restart=on-failure
RestartSec=3
TimeoutStopSec=90
UMask=0077
NoNewPrivileges=true
ProtectSystem=strict
ReadWritePaths=$root
# Home and common server locations stay visible; ACLs govern what vesta may read.
ProtectHome=false
PrivateTmp=true
PrivateDevices=true
RestrictAddressFamilies=AF_UNIX AF_INET AF_INET6 AF_NETLINK
ProtectKernelTunables=true
ProtectKernelModules=true
ProtectControlGroups=true
RestrictSUIDSGID=true
LockPersonality=true
RestrictRealtime=true
CapabilityBoundingSet=
# First pass: host and games share vesta. Split credentials/accounts in H11.
# No MemoryDenyWriteExecute: Java JIT requires executable memory.
[Install]
WantedBy=multi-user.target
EOF
}
mode=${1:-}; shift || true
if [[ $mode == resume-decision ]]; then
 if [[ -f $1/host.toml ]]; then echo preserve; else echo create; fi
 exit
fi
if [[ $mode == print-unit ]]; then root=$1; unit; exit; fi
removal=
declare -A explicit=()
root=/var/lib/vesta; port=25500; source_dir=; local_only=false; start=true; service=true; answers=; yes=false; json=false
if [[ $mode == install ]]; then binaries=${1:?}; shift; fi
import_action=
if [[ $mode == import-root ]]; then
  import_action=${1:-}
  case $import_action in
    add|list|remove) shift;;
    *) fail 'Usage: vesta import-root add <path> | list | remove <path>  [--root /var/lib/vesta] [--yes] [--json]';;
  esac
  if [[ $import_action == add || $import_action == remove ]]; then source_dir=${1:?}; shift; fi
fi

while (($#)); do
 case $1 in
 --keep-data|--remove-data) [[ -z $removal ]] || fail "Choose one data-removal option"; removal=$1; shift;;
 --root) explicit[root]=true; root=${2:?}; shift 2;; --port) explicit[port]=true; port=${2:?}; shift 2;; --server-dir) explicit[server_dir]=true; source_dir=${2:?}; shift 2;;
 --local-only) explicit[local_only]=true; local_only=true; shift;; --no-start) start=false; shift;; --no-service) service=false; start=false; shift;;
 --answers) answers=${2:?}; shift 2;; --yes) yes=true; shift;; --json) json=true; shift;; *) fail "Unknown option $1";;
 esac
done
if [[ $mode == import-root || $mode == uninstall ]] && [[ ${explicit[root]:-} != true && -f /usr/local/lib/vesta/data-root ]]; then
 root=$(cat /usr/local/lib/vesta/data-root)
fi
if [[ -n $answers ]]; then
 command -v python3 >/dev/null || fail 'python3 required for JSON answers.'
 # JSON keys match CLI names with underscores. Explicit CLI options take precedence.
 parsed_answers=$(python3 -c 'import json,sys; d=json.load(open(sys.argv[1])); allowed={"root","port","server_dir","local_only","no_start","no_service"}; assert not (d.keys()-allowed), "Unknown answer"; assert all(isinstance(d[k],bool) for k in ("local_only","no_start","no_service") if k in d); assert all(isinstance(d[k],str) for k in ("root","server_dir") if k in d); assert "port" not in d or type(d["port"]) is int; assert all("\n" not in str(v) for v in d.values()); [print(k+"="+(str(v).lower() if isinstance(v,bool) else str(v))) for k,v in d.items()]' "$answers") || fail "Invalid answers file"
 while IFS= read -r line; do
  key=${line%%=*}; value=${line#*=}
  case $key in
  root) [[ ${explicit[root]:-} == true ]] || root=$value;; port) [[ ${explicit[port]:-} == true ]] || port=$value;;
  server_dir) [[ ${explicit[server_dir]:-} == true ]] || source_dir=$value;; local_only) [[ ${explicit[local_only]:-} == true ]] || local_only=$value;;
  no_start) [[ $value != true ]] || start=false;; no_service) [[ $value != true ]] || { service=false; start=false; };;
  esac
 done <<< "$parsed_answers"
fi
[[ $local_only == true || $local_only == false ]] || fail "local_only must be boolean."
[[ $port =~ ^[0-9]{1,5}$ ]] && ((10#$port > 0 && 10#$port < 65536)) || fail 'Port must be 1..65535.'
port=$((10#$port))
# Restrict paths embedded in systemd/TOML/shell wrapper. Spaces are supported in import paths.
[[ $root =~ ^/[a-zA-Z0-9_/-]+$ && $root != / && $root != */../* && $root != */.. && $root != */./* ]] || fail 'Use an absolute data directory containing letters, digits, slash, dash or underscore.'
# Reject links in the data directory chain before provisioning/removing files.
parent=$root
while [[ $parent != / ]]; do [[ ! -L $parent ]] || fail "Symlink data directory refused: $parent"; parent=$(dirname -- "$parent"); done
root=$(realpath -m -- "$root")
[[ $root != / && $root != /usr && $root != /etc && $root != /var && $root != /home && $root != /var/lib && $root != /usr/local && $root != /tmp && $root != /srv ]] || fail 'Unsafe data directory.'
[[ $(id -u) == 0 ]] || fail 'Requires root; run with sudo.'
if [[ $mode == uninstall ]]; then
 [[ $removal == --keep-data || $removal == --remove-data ]] || fail 'Choose uninstall --keep-data or --remove-data explicitly.'
 [[ $(cat /usr/local/lib/vesta/data-root 2>/dev/null) == "$root" ]] || fail 'This directory is not the installed Vesta Server.'
 if [[ -f /etc/systemd/system/vesta.service && -d /run/systemd/system ]]; then systemctl disable --now vesta.service; fi
 pkill -TERM -u vesta -x vesta-agent 2>/dev/null || true
 rm -f /etc/systemd/system/vesta.service /usr/local/bin/vesta
 rm -rf /usr/local/lib/vesta
 [[ ! -d /run/systemd/system ]] || systemctl daemon-reload
 if [[ $removal == --remove-data ]]; then rm -rf -- "$root"; else echo "Data retained in $root"; fi
 echo 'Vesta Server uninstalled. The vesta account and import ACLs are retained; review them before removing.'
 exit
fi
if [[ $mode == import-root ]]; then
 [[ -f $root/host.toml ]] || fail 'No installed host.toml at this data directory.'
 roots_file=$root/import-roots.json
 if [[ $import_action == list ]]; then
  python3 - "$roots_file" "$json" <<'PY'
import json,sys
from pathlib import Path
p=Path(sys.argv[1]); as_json=sys.argv[2]=='true'
roots=json.loads(p.read_text()) if p.exists() else []
if not isinstance(roots,list): roots=[]
roots=[r for r in roots if isinstance(r,str)]
if as_json:
 print(json.dumps({"roots":[{"path":r} for r in roots]},separators=(',',':')))
elif not roots:
 print('No import folders registered.')
else:
 print('Import folders')
 print()
 for r in roots:
  print(f'  {r}')
PY
  exit
 fi
 source_dir=$(realpath -e -- "$source_dir")
 [[ -d $source_dir && $source_dir != / ]] || fail 'Choose an existing import directory, not /.'
 if [[ $import_action == remove ]]; then
  python3 - "$roots_file" "$source_dir" <<'PY' || fail 'That path is not a registered import folder.'
import json,sys
from pathlib import Path
p=Path(sys.argv[1]); target=sys.argv[2]
a=json.loads(p.read_text()) if p.exists() else []
sys.exit(0 if isinstance(a,list) and target in a else 1)
PY
  command -v setfacl >/dev/null || fail 'Install ACL tools first: sudo apt install acl'
  echo "Will revoke vesta access from: $source_dir"
  if [[ $yes == false && -t 0 ]]; then
   read -r -p 'Revoke these permissions and unregister the folder? [y/N] ' answer
   [[ $answer == y || $answer == Y || $answer == yes ]] || fail 'No permissions changed.'
  fi
  setfacl -R -P -x u:vesta -- "$source_dir" 2>/dev/null || true
  python3 - "$roots_file" "$source_dir" <<'PY'
import json,sys
from pathlib import Path
p=Path(sys.argv[1]); target=sys.argv[2]
a=[r for r in (json.loads(p.read_text()) if p.exists() else []) if isinstance(r,str) and r!=target]
# Drop traverse ACL on parents only when no remaining root still needs them.
remaining=a
parent=str(Path(target).parent)
while parent!='/':
 need=any(r==parent or r.startswith(parent.rstrip('/')+'/') for r in remaining)
 if not need:
  import subprocess
  subprocess.run(['setfacl','-x','u:vesta',parent],check=False)
 parent=str(Path(parent).parent)
t=p.with_suffix('.tmp'); t.write_text(json.dumps(a)); t.chmod(0o644); t.replace(p)
sug=Path(sys.argv[1]).parent/'suggested-import.json'
if sug.exists():
 try:
  if json.loads(sug.read_text()).get('path')==target: sug.unlink()
 except Exception: pass
PY
  if [[ $json == true ]]; then
   echo "{\"removed\":\"$source_dir\"}"
  else
   echo "Import root removed: $source_dir"
   echo 'Restarting Vesta Server so the launcher drops this folder.'
  fi
  if [[ -f /etc/systemd/system/vesta.service ]]; then systemctl try-restart vesta.service; fi
  exit
 fi
 # add
 for protected in "$root" /usr/local/lib/vesta; do
  [[ $source_dir != "$protected" && $source_dir != "$protected"/* && $protected != "$source_dir"/* ]] || fail "Import directory overlaps Vesta Server files: $protected"
 done
 command -v setfacl >/dev/null || fail 'Install ACL tools first: sudo apt install acl'
 echo "Will grant vesta read/traverse access recursively to: $source_dir"
 echo 'Will grant traverse-only access on each parent. No write access is granted.'
 if [[ $yes == false && -t 0 ]]; then
  read -r -p 'Apply these permissions? [y/N] ' answer
  [[ $answer == y || $answer == Y || $answer == yes ]] || fail 'No permissions changed.'
 fi
 setfacl -R -P -m u:vesta:rX -- "$source_dir"
 parent=$(dirname -- "$source_dir")
 while [[ $parent != / ]]; do setfacl -m u:vesta:--x -- "$parent"; parent=$(dirname -- "$parent"); done
 python3 - "$root" "$source_dir" <<'PY'
import json,sys
from pathlib import Path
p=Path(sys.argv[1])/"import-roots.json"
a=json.loads(p.read_text()) if p.exists() else []
if sys.argv[2] not in a: a.append(sys.argv[2])
suggestion=Path(sys.argv[1])/"suggested-import.json"
suggestion.write_text(json.dumps({"path":sys.argv[2]})); suggestion.chmod(0o644)
t=p.with_suffix('.tmp'); t.write_text(json.dumps(a)); t.chmod(0o644); t.replace(p)
PY
 if [[ $json == true ]]; then
  echo "{\"added\":\"$source_dir\"}"
 else
  echo "Import root added: $source_dir"
  echo 'Restarting Vesta Server so the launcher can use this folder.'
 fi
 if [[ -f /etc/systemd/system/vesta.service ]]; then systemctl try-restart vesta.service; fi
 exit
fi
[[ $mode == install ]] || fail 'Unknown operation.'
for name in vesta vesta-agent vesta-setup vesta-supervisor; do [[ -f $binaries/$name && -x $binaries/$name ]] || fail "Missing executable $binaries/$name"; done
command -v python3 >/dev/null || fail 'Install python3 first.'
[[ $service == false || -d /run/systemd/system ]] || fail 'systemd is required (or choose --no-service).'
# Never reuse a login account or change an existing unrelated user.
if id vesta >/dev/null 2>&1; then
 [[ $(id -u vesta) != 0 ]] || fail "Refusing privileged vesta account"
 shell=$(getent passwd vesta | cut -d: -f7)
 [[ $shell == */nologin || $shell == */false ]] || fail 'Existing vesta account has a login shell; refusing to reuse it.'
else useradd --system --user-group --home-dir "$root" --shell /usr/sbin/nologin vesta; fi
if [[ -e /usr/local/lib/vesta/data-root ]]; then
 [[ $(cat /usr/local/lib/vesta/data-root) == "$root" ]] || fail 'A different Vesta data directory is installed; uninstall it first.'
 if [[ $service == true && -f /etc/systemd/system/vesta.service ]]; then systemctl stop vesta.service; fi
fi
install -d -m 755 /usr/local/lib/vesta
install -d -m 750 -o root -g vesta "$root"
install -d -m 700 -o vesta -g vesta "$root/host" "$root/servers" "$root/java"
# Install through rename so a running executable is never overwritten in place.
for name in vesta vesta-agent vesta-setup vesta-supervisor; do
 install -m 755 -o root -g root "$binaries/$name" "/usr/local/lib/vesta/$name.new"
 mv -f "/usr/local/lib/vesta/$name.new" "/usr/local/lib/vesta/$name"
done
printf '%s\n' "$root" > /usr/local/lib/vesta/data-root
# Generate config/identity only once; reruns preserve edits and all host state.
if [[ $(bash -c '[[ -f $1/host.toml ]] && echo preserve || echo create' -- "$root") == create ]]; then
 /usr/local/lib/vesta/vesta-setup linux-identity "$root"
 interfaces=$(ip -o -4 addr show scope global | awk '$2 != "lo" {print $2}' | sort -u)
 ROOT=$root PORT=$port LOCAL_ONLY=$local_only INTERFACES=$interfaces python3 - <<'PY'
import json,os
from pathlib import Path
q=json.dumps
r=Path(os.environ['ROOT']); local=os.environ['LOCAL_ONLY']=='true'
c='state = "host"\ncertificate = "certificate.pem"\nprivate-key = "key.pem"\n'
c+=f'port = {os.environ["PORT"]}\nlocal-only = {str(local).lower()}\nbind = {q("127.0.0.1" if local else "0.0.0.0")}\n'
c+='discovery-hostname = '+q(__import__('socket').gethostname().split('.')[0]+'.local.')+'\n'
c+='discovery-interfaces = '+q(os.environ['INTERFACES'].split())+'\n'
(r/'host.toml').write_text(c)
PY
 chmod 644 "$root/certificate.pem"; chmod 640 "$root/host.toml" "$root/key.pem"; chown root:vesta "$root/host.toml" "$root/key.pem"
fi
# Existing configuration wins on rerun; read the two keys this installer writes.
requested_port=$port requested_local_only=$local_only
port=$(sed -n 's/^port *= *\([0-9]\{1,5\}\) *$/\1/p' "$root/host.toml" | tail -n1); port=${port:-25500}
local_only=$(sed -n 's/^local-only *= *\(true\|false\) *$/\1/p' "$root/host.toml" | tail -n1); local_only=${local_only:-false}
if [[ ${explicit[port]:-} == true && $requested_port != "$port" || ${explicit[local_only]:-} == true && $requested_local_only != "$local_only" ]]; then
 echo "NOTE: keeping the existing $root/host.toml (port $port, local-only $local_only). Edit it and run: systemctl restart vesta" >&2
fi
cat > /usr/local/bin/vesta <<EOF
#!/bin/sh
export VESTA_HOST_STATE='$root/host'
exec /usr/local/lib/vesta/vesta "\$@"
EOF
chmod 755 /usr/local/bin/vesta
if [[ $local_only == false ]]; then
 if command -v ufw >/dev/null && ufw status | grep -q '^Status: active'; then
  # Scope to the directly attached IPv4 networks rather than any source.
  for net in $(ip -o -4 addr show scope global | awk '$2 != "lo" {print $4}' | python3 -c 'import ipaddress,sys; [print(n) for n in sorted({str(ipaddress.ip_interface(l.strip()).network) for l in sys.stdin if l.strip()})]'); do
   ufw allow proto tcp from "$net" to any port "$port" comment 'Vesta management'
   ufw allow proto udp from "$net" to any port 5353 comment 'Vesta LAN discovery'
  done
 fi
 if command -v firewall-cmd >/dev/null && firewall-cmd --state >/dev/null 2>&1; then
  firewall-cmd --permanent --add-port="$port/tcp"; firewall-cmd --add-port="$port/tcp"
  firewall-cmd --permanent --add-service=mdns; firewall-cmd --add-service=mdns
 fi
fi
if [[ $service == true ]]; then
 unit > /etc/systemd/system/vesta.service
 chmod 644 /etc/systemd/system/vesta.service
 systemctl daemon-reload; systemctl enable vesta.service
 [[ $start == false ]] || systemctl start vesta.service
fi
# --server-dir is an explicit ACL grant request; reuse the same local admin operation.
if [[ -n $source_dir ]]; then /usr/local/bin/vesta import-root add "$source_dir" --root "$root" --yes; fi
if [[ $start == true ]]; then
 ready=false
 for ((i=0;i<100;i++)); do
  if status=$(/usr/local/bin/vesta host-status --json 2>/dev/null); then ready=true; break; fi
  sleep .1
 done
 [[ $ready == true ]] || fail 'Local control channel unavailable; inspect journalctl -u vesta.'
 claimed=$(python3 -c 'import json,sys; print(str(json.loads(sys.argv[1])["claimed"]).lower())' "$status")
 if [[ $claimed == true ]]; then
  echo 'Vesta Server is already claimed. To add another device, run: sudo vesta link'
 else
  echo "Vesta Server is running on $(hostname -s)."
  /usr/local/bin/vesta setup-code || fail 'Service could not issue a setup code.'
 fi
else echo "Vesta Server installed. Start with: systemctl start vesta (or runuser -u vesta -- /usr/local/lib/vesta/vesta-agent host $root/host.toml)."; fi
echo 'Game ports need firewall rules when servers are created/imported; allow the selected game TCP port before players connect.'

