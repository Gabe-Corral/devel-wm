#!/usr/bin/env bash
# Run the nested compositor and its prototype layer-shell bar together.
set -euo pipefail

root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
: "${XDG_RUNTIME_DIR:?XDG_RUNTIME_DIR must be set to run devel-wm}"

log=$(mktemp "$XDG_RUNTIME_DIR/devel-wm-launch.XXXXXX")
compositor_pid=
bar_pid=

cleanup() {
    trap - EXIT INT TERM
    if [[ -n $bar_pid ]]; then
        kill "$bar_pid" 2>/dev/null || true
        wait "$bar_pid" 2>/dev/null || true
    fi
    if [[ -n $compositor_pid ]]; then
        kill "$compositor_pid" 2>/dev/null || true
        wait "$compositor_pid" 2>/dev/null || true
    fi
    rm -f -- "$log"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

cargo build --locked --manifest-path "$root/Cargo.toml"
cargo build --locked --manifest-path "$root/bar-test/Cargo.toml"

# tee preserves startup diagnostics and keeps the compositor's later logs visible.
"$root/target/debug/devel-wm" "$@" > >(tee "$log") 2>&1 &
compositor_pid=$!

deadline=$((SECONDS + 15))
wayland_display=
ipc_socket=
while (( SECONDS < deadline )); do
    display_line=$(grep -m1 '^WAYLAND_DISPLAY=' "$log" || true)
    ipc_line=$(grep -m1 '^DEVEL_WM_IPC=' "$log" || true)
    if [[ -n $display_line && -n $ipc_line ]]; then
        wayland_display=${display_line#WAYLAND_DISPLAY=}
        ipc_socket=${ipc_line#DEVEL_WM_IPC=}
        if [[ -S $ipc_socket && -S $XDG_RUNTIME_DIR/$wayland_display ]]; then
            break
        fi
    fi
    if ! kill -0 "$compositor_pid" 2>/dev/null; then
        echo "devel-wm exited before the bar could start" >&2
        exit 1
    fi
    sleep 0.1
done

if [[ -z $wayland_display || -z $ipc_socket || ! -S $ipc_socket || ! -S $XDG_RUNTIME_DIR/$wayland_display ]]; then
    echo "Timed out waiting for devel-wm's Wayland and IPC sockets" >&2
    exit 1
fi

env -u DISPLAY WAYLAND_DISPLAY="$wayland_display" DEVEL_WM_IPC="$ipc_socket" \
    "$root/bar-test/target/debug/prototype-bar" &
bar_pid=$!
echo "Prototype bar running on $wayland_display (IPC: $ipc_socket)"
echo "Close the nested window or press Ctrl-C to stop both processes."

status=0
wait -n "$compositor_pid" "$bar_pid" || status=$?
exit "$status"
