#!/usr/bin/env bash
# Auto-discover native outputs and run one prototype bar from a Linux text VT.
set -euo pipefail

root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)

usage() {
    cat <<'EOF'
Usage: scripts/run-native-with-bar.sh [--config PATH]

From a separate Linux text VT, discover connected outputs on one DRM card and
launch them with one prototype bar. --backend native alone is still probe-only.
Running this script explicitly opts into an unlimited native session.
Ctrl-C requests the compositor's graceful shutdown; never use kill -9.
EOF
}

config=()
while (($#)); do
    case $1 in
        --config)
            if (($# < 2)) || [[ -z $2 ]]; then
                echo '--config requires a TOML file path' >&2
                exit 2
            fi
            config=(--config "$2")
            shift 2
            ;;
        --help|-h) usage; exit 0 ;;
        *) echo "Unknown option: $1" >&2; usage >&2; exit 2 ;;
    esac
done

: "${XDG_RUNTIME_DIR:?XDG_RUNTIME_DIR must be set to run devel-wm}"
tty_path=$(readlink /proc/self/fd/0 || true)
if [[ ! -t 0 || ! $tty_path =~ ^/dev/tty([1-9]|[1-5][0-9]|6[0-3])$ ]]; then
    echo 'Native devel-wm requires a separate Linux text VT (/dev/tty1–63), not an X11/i3 or Wayland terminal.' >&2
    echo 'Switch to an unused VT (for example Ctrl+Alt+F3), log in, then run this script there.' >&2
    echo 'No DRM modeset or bar was started.' >&2
    exit 2
fi
# Build before acquiring DRM master. The compositor itself refuses native mode
# outside a Linux text VT as a second safety check.
cargo build --locked --features native --manifest-path "$root/Cargo.toml"
cargo build --locked --manifest-path "$root/bar-test/Cargo.toml"

log=$(mktemp "$XDG_RUNTIME_DIR/devel-wm-native-launch.XXXXXX")
compositor_pid=
bar_pid=
compositor_reaped=false

cleanup() {
    trap - EXIT INT TERM
    if [[ -n $bar_pid ]]; then
        kill -TERM "$bar_pid" 2>/dev/null || true
        wait "$bar_pid" 2>/dev/null || true
    fi
    if [[ -n $compositor_pid && $compositor_reaped == false ]]; then
        # SIGINT is handled by devel-wm: wait for scoped DRM/VT restoration.
        kill -INT "$compositor_pid" 2>/dev/null || true
        wait "$compositor_pid" 2>/dev/null || true
    fi
    rm -f -- "$log"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

# Bash backgrounds a command with stdin on /dev/null when job control is off.
# Keep a duplicate of the already-open login VT: reopening /dev/ttyN can fail
# even when the shell has permission to use its existing descriptor.
exec {vt_fd}<&0
"$root/target/debug/devel-wm" --backend native --native-session "${config[@]}" \
    <&"$vt_fd" > >(tee "$log") 2>&1 &
compositor_pid=$!
exec {vt_fd}<&-

wayland_display=
ipc_socket=
deadline=$((SECONDS + 15))
while ((SECONDS < deadline)); do
    display_line=$(grep -m1 '^WAYLAND_DISPLAY=' "$log" || true)
    ipc_line=$(grep -m1 '^DEVEL_WM_IPC=' "$log" || true)
    if [[ -n $display_line && -n $ipc_line ]]; then
        wayland_display=${display_line#WAYLAND_DISPLAY=}
        ipc_socket=${ipc_line#DEVEL_WM_IPC=}
        if [[ $wayland_display =~ ^wayland-[0-9]+$ \
              && $ipc_socket == "$XDG_RUNTIME_DIR/devel-wm/"* \
              && -S $XDG_RUNTIME_DIR/$wayland_display && -S $ipc_socket ]]; then
            break
        fi
    fi
    if ! kill -0 "$compositor_pid" 2>/dev/null; then
        status=0
        wait "$compositor_pid" || status=$?
        compositor_reaped=true
        echo "devel-wm exited before the bar could start (status: $status)" >&2
        if ((status == 0)); then exit 1; else exit "$status"; fi
    fi
    sleep 0.1
done
if [[ ! $wayland_display =~ ^wayland-[0-9]+$ \
      || $ipc_socket != "$XDG_RUNTIME_DIR/devel-wm/"* \
      || ! -S $XDG_RUNTIME_DIR/$wayland_display || ! -S $ipc_socket ]]; then
    echo 'Timed out waiting for native Wayland and IPC sockets' >&2
    exit 1
fi

env -u DISPLAY -u WAYLAND_SOCKET WAYLAND_DISPLAY="$wayland_display" \
    DEVEL_WM_IPC="$ipc_socket" "$root/bar-test/target/debug/prototype-bar" &
bar_pid=$!
echo "Prototype bar started on $wayland_display (IPC: $ipc_socket)"
echo 'Use Ctrl+Alt+Backspace to exit devel-wm; Ctrl-C here also requests graceful exit.'

finished=
wait -n -p finished "$compositor_pid" "$bar_pid" || true
if [[ $finished == "$bar_pid" ]]; then
    # tee may still be copying the compositor's shutdown line when the bar's
    # disconnect is observed. Give it a short chance to catch up.
    for ((attempt=0; attempt<10; attempt++)); do
        if grep -q '^Native session stopping;' "$log"; then break; fi
        sleep 0.1
    done
    if grep -q '^Native session stopping;' "$log"; then
        echo 'Prototype bar disconnected during native shutdown'
    else
        echo 'Prototype bar exited before devel-wm began shutdown; requesting native cleanup' >&2
        kill -INT "$compositor_pid" 2>/dev/null || true
        wait "$compositor_pid" 2>/dev/null || true
        compositor_reaped=true
        exit 1
    fi
fi
status=0
wait "$compositor_pid" || status=$?
compositor_reaped=true
echo "Native compositor exit status: $status"
exit "$status"
