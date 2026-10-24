# Bash rc file for cargo dev's child shell. No persistent PATH or profile edits.
_vesta_dev_update_path() {
    local next part rest filtered=''
    IFS= read -r next < "$VESTA_DEV_SESSION/current-bin" || [[ -n $next ]] || return 1
    for part in vesta vesta-agent vesta-supervisor vesta-setup; do
        [[ -x "$next/$part" ]] || { printf 'Incomplete development build\n' >&2; return 1; }
    done
    rest=$PATH
    while :; do
        part=${rest%%:*}
        if [[ -n $part && $part != "${VESTA_DEV_BIN:-}" ]]; then filtered+="${filtered:+:}$part"; fi
        [[ $rest == *:* ]] || break
        rest=${rest#*:}
    done
    export PATH="$next${filtered:+:$filtered}" VESTA_DEV_BIN="$next"
    hash -r
}
vesta-rebuild() {
    local args=(build --session "$VESTA_DEV_SESSION")
    [[ $VESTA_DEV_RELEASE != 1 ]] || args+=(--release)
    args+=("$@")
    local result=0
    "$VESTA_DEV_TOOL" "${args[@]}" || result=$?
    _vesta_dev_update_path || return
    [[ $result == 0 ]] || return "$result"
    printf 'Commands now use the rebuilt binaries. Only an explicitly selected --restart server is restarted.\n'
}
_vesta_dev_update_path || return
printf 'Vesta dev session | registry: %s\n' "$VESTA_SERVER_ROOT"
printf 'vesta list | vesta manage <id> | vesta-setup preview | vesta-rebuild [--restart <id>] | exit\n'
