#!/usr/bin/env bash
# Run the nested compositor and devel-bar together.
set -euo pipefail

root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)

usage() {
    cat <<'EOF'
Usage: scripts/run-with-bar.sh [--alacritty COUNT] [--config PATH]

Launch the nested compositor and devel-bar, optionally with COUNT Alacritty windows.
EOF
}

alacritty_count=0
compositor_args=()
while (($#)); do
    case $1 in
        --alacritty)
            if (($# < 2)) || [[ ! $2 =~ ^[0-9]{1,3}$ ]]; then
                echo "--alacritty requires a non-negative window count (up to 32)" >&2
                exit 2
            fi
            alacritty_count=$((10#$2))
            if ((alacritty_count > 32)); then
                echo "--alacritty supports at most 32 windows" >&2
                exit 2
            fi
            shift 2
            ;;
        --config)
            if (($# < 2)) || [[ -z $2 ]]; then
                echo "--config requires a TOML file path" >&2
                exit 2
            fi
            compositor_args+=("$1" "$2")
            shift 2
            ;;
        --help|-h)
            usage
            exit 0
            ;;
        *)
            echo "Unknown option: $1" >&2
            usage >&2
            exit 2
            ;;
    esac
done

: "${XDG_RUNTIME_DIR:?XDG_RUNTIME_DIR must be set to run devel-wm}"
if ((alacritty_count > 0)) && ! command -v alacritty >/dev/null 2>&1; then
    echo "Alacritty is required for --alacritty" >&2
    exit 1
fi

log=$(mktemp "$XDG_RUNTIME_DIR/devel-wm-launch.XXXXXX")
compositor_pid=
bar_pid=
alacritty_pids=()
wayland_display=
ipc_socket=
wayland_socket_inode=
ipc_socket_inode=

cleanup() {
    trap - EXIT INT TERM
    for pid in "${alacritty_pids[@]}"; do
        kill "$pid" 2>/dev/null || true
    done
    for pid in "${alacritty_pids[@]}"; do
        wait "$pid" 2>/dev/null || true
    done
    if [[ -n $bar_pid ]]; then
        kill "$bar_pid" 2>/dev/null || true
        wait "$bar_pid" 2>/dev/null || true
    fi
    if [[ -n $compositor_pid ]]; then
        kill "$compositor_pid" 2>/dev/null || true
        wait "$compositor_pid" 2>/dev/null || true
    fi
    # SIGTERM does not run the compositor's Rust destructors; remove only the
    # sockets belonging to this launcher after its compositor has stopped.
    if [[ -n $ipc_socket_inode && $ipc_socket == "$XDG_RUNTIME_DIR/devel-wm/"* \
          && -S $ipc_socket && $(stat -Lc '%d:%i' -- "$ipc_socket") == "$ipc_socket_inode" ]]; then
        rm -f -- "$ipc_socket"
    fi
    if [[ -n $wayland_socket_inode && $wayland_display =~ ^wayland-[0-9]+$ \
          && -S $XDG_RUNTIME_DIR/$wayland_display \
          && -f $XDG_RUNTIME_DIR/$wayland_display.lock ]]; then
        (
            exec {lock_fd}<"$XDG_RUNTIME_DIR/$wayland_display.lock"
            if flock -n "$lock_fd" \
                && [[ $(stat -Lc '%d:%i' -- "$XDG_RUNTIME_DIR/$wayland_display") == "$wayland_socket_inode" ]]; then
                rm -f -- "$XDG_RUNTIME_DIR/$wayland_display" "$XDG_RUNTIME_DIR/$wayland_display.lock"
            fi
        )
    fi
    rm -f -- "$log"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

cargo build --locked --manifest-path "$root/Cargo.toml"
cargo build --locked --manifest-path "$root/../devel-bar/Cargo.toml"

# tee preserves startup diagnostics and keeps the compositor's later logs visible.
"$root/target/debug/devel-wm" "${compositor_args[@]}" > >(tee "$log") 2>&1 &
compositor_pid=$!

deadline=$((SECONDS + 15))
while (( SECONDS < deadline )); do
    display_line=$(grep -m1 '^WAYLAND_DISPLAY=' "$log" || true)
    ipc_line=$(grep -m1 '^DEVEL_WM_IPC=' "$log" || true)
    if [[ -n $display_line && -n $ipc_line ]]; then
        wayland_display=${display_line#WAYLAND_DISPLAY=}
        ipc_socket=${ipc_line#DEVEL_WM_IPC=}
        if [[ -S $ipc_socket && -S $XDG_RUNTIME_DIR/$wayland_display ]]; then
            ipc_socket_inode=$(stat -Lc '%d:%i' -- "$ipc_socket")
            wayland_socket_inode=$(stat -Lc '%d:%i' -- "$XDG_RUNTIME_DIR/$wayland_display")
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
    "$root/../devel-bar/target/debug/devel-bar" &
bar_pid=$!
echo "devel-bar running on $wayland_display (IPC: $ipc_socket)"
for ((i = 0; i < alacritty_count; i++)); do
    env -u DISPLAY WAYLAND_DISPLAY="$wayland_display" DEVEL_WM_IPC="$ipc_socket" alacritty &
    alacritty_pids+=("$!")
done
if ((alacritty_count > 0)); then
    echo "Launched $alacritty_count Alacritty window(s) on $wayland_display"
fi
echo "Close the nested window or press Ctrl-C to stop all launched processes."

status=0
wait -n "$compositor_pid" "$bar_pid" || status=$?
exit "$status"
