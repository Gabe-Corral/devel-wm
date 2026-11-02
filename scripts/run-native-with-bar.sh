#!/usr/bin/env bash
# Auto-discover native outputs and run devel-bar from a Linux text VT.
set -euo pipefail

root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)

usage() {
    cat <<'EOF'
Usage: scripts/run-native-with-bar.sh [--config PATH] [--release] [--perf-sample]

From a separate Linux text VT, discover connected outputs on one DRM card and
launch them with devel-bar. --backend native alone is still probe-only.
Running this script explicitly opts into an unlimited native session.
--release builds and runs an optimized devel-wm (bar/menu remain debug builds).
--perf-sample records devel-wm CPU ticks and RSS once per second.
Ctrl-C requests the compositor's graceful shutdown; never use kill -9.
EOF
}

config=()
compositor_profile=debug
compositor_build_flags=()
perf_sample=false
while (($#)); do
    case $1 in
        --perf-sample)
            perf_sample=true
            shift
            ;;
        --release)
            compositor_profile=release
            compositor_build_flags=(--release)
            shift
            ;;
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
cargo build --locked --features native "${compositor_build_flags[@]}" --manifest-path "$root/Cargo.toml"
cargo build --locked --manifest-path "$root/../devel-bar/Cargo.toml"
cargo build --locked --manifest-path "$root/../devel-menu/Cargo.toml"

log=$(mktemp "$XDG_RUNTIME_DIR/devel-wm-native-launch.XXXXXX")
compositor_pid=
bar_pid=
sampler_pid=
sample_log=
compositor_reaped=false

sample_process() {
    local pid=$1 stat rss peak
    local -a fields
    while [[ -r /proc/$pid/stat && -r /proc/$pid/status ]]; do
        stat=$(</proc/"$pid"/stat) || break
        read -r -a fields <<< "${stat##*) }"
        read -r rss peak < <(awk '
            /^VmRSS:/ {rss=$2}
            /^VmHWM:/ {peak=$2}
            END {if (rss != "" && peak != "") print rss, peak}
        ' "/proc/$pid/status") || break
        [[ -n $rss && -n $peak ]] || break
        printf 'PERF_PROCESS pid=%s cpu_ticks=%s rss_kib=%s peak_rss_kib=%s\n' \
            "$pid" "$((fields[11] + fields[12]))" "$rss" "$peak" >> "$sample_log"
        sleep 1
    done
}

cleanup() {
    trap - EXIT INT TERM
    if [[ -n $sampler_pid ]]; then
        kill -TERM "$sampler_pid" 2>/dev/null || true
        wait "$sampler_pid" 2>/dev/null || true
    fi
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
DEVEL_MENU_BIN="$root/../devel-menu/target/debug/devel-menu" \
    "$root/target/$compositor_profile/devel-wm" --backend native --native-session "${config[@]}" \
    <&"$vt_fd" > >(tee "$log") 2>&1 &
compositor_pid=$!
exec {vt_fd}<&-

wayland_display=
ipc_socket=
control_socket=
deadline=$((SECONDS + 15))
while ((SECONDS < deadline)); do
    display_line=$(grep -m1 '^WAYLAND_DISPLAY=' "$log" || true)
    ipc_line=$(grep -m1 '^DEVEL_WM_IPC=' "$log" || true)
    control_line=$(grep -m1 '^DEVEL_WM_CONTROL=' "$log" || true)
    if [[ -n $display_line && -n $ipc_line && -n $control_line ]]; then
        wayland_display=${display_line#WAYLAND_DISPLAY=}
        ipc_socket=${ipc_line#DEVEL_WM_IPC=}
        control_socket=${control_line#DEVEL_WM_CONTROL=}
        if [[ $wayland_display =~ ^wayland-[0-9]+$ \
              && $ipc_socket == "$XDG_RUNTIME_DIR/devel-wm/"* \
              && $control_socket == "$XDG_RUNTIME_DIR/devel-wm/"* \
              && -S $XDG_RUNTIME_DIR/$wayland_display && -S $ipc_socket && -S $control_socket ]]; then
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
       || $control_socket != "$XDG_RUNTIME_DIR/devel-wm/"* \
       || ! -S $XDG_RUNTIME_DIR/$wayland_display || ! -S $ipc_socket || ! -S $control_socket ]]; then
    echo 'Timed out waiting for native Wayland, IPC and control sockets' >&2
    exit 1
fi

env -u DISPLAY -u WAYLAND_SOCKET WAYLAND_DISPLAY="$wayland_display" \
    DEVEL_WM_IPC="$ipc_socket" DEVEL_WM_CONTROL="$control_socket" \
    "$root/../devel-bar/target/debug/devel-bar" &
bar_pid=$!
echo "devel-bar started on $wayland_display (one surface per named output; IPC: $ipc_socket)"
if [[ $perf_sample == true ]]; then
    # Keep sampling output separate: concurrent writes to the compositor's
    # stdout/stderr can interleave in the caller's redirected session log.
    sample_log=$(mktemp "$XDG_RUNTIME_DIR/devel-wm-native-perf.$compositor_pid.XXXXXX")
    echo "PERF_PROCESS_LOG=$sample_log"
    sample_process "$compositor_pid" &
    sampler_pid=$!
fi
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
        echo 'devel-bar disconnected during native shutdown'
    else
        echo 'devel-bar exited before devel-wm began shutdown; requesting native cleanup' >&2
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
