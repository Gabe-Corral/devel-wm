#!/usr/bin/env python3
"""Repeatable nested compositor benchmark (needs an X11 host and Alacritty).

Uses the same client activity for baseline and candidate runs. Results are
wall-clock/CPU observations, not GPU hardware timestamps.
"""

import argparse
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import time


ROOT = Path(__file__).resolve().parent.parent


def cpu_seconds(pid):
    stat = Path(f"/proc/{pid}/stat").read_text()
    fields = stat[stat.rfind(")") + 2 :].split()
    return (int(fields[11]) + int(fields[12])) / os.sysconf("SC_CLK_TCK")


def stop(process):
    if process is None or process.poll() is not None:
        return
    process.terminate()
    try:
        process.wait(timeout=3)
    except subprocess.TimeoutExpired:
        process.kill()
        process.wait(timeout=3)


def metrics(text):
    records = []
    for line in text.splitlines():
        if not line.startswith("PERF "):
            continue
        records.append({key: int(value) for key, value in re.findall(r"(\w+)=(\d+)", line)})
    totals = {key: sum(row.get(key, 0) for row in records) for key in (
        "interval_ms", "events", "commits", "visible_commits", "refresh_us",
        "cleanup_us", "ipc_us", "redraw_requests", "frames", "submitted",
        "render_us", "submit_us", "damaged_pixels", "output_pixels",
    )}
    totals["frame_p95_us_max_interval"] = max((row.get("frame_p95_us", 0) for row in records), default=0)
    totals["damage_fraction"] = (totals["damaged_pixels"] / totals["output_pixels"]
                                 if totals["output_pixels"] else None)
    totals["render_submit_us_per_frame"] = ((totals["render_us"] + totals["submit_us"]) / totals["frames"]
                                            if totals["frames"] else None)
    return totals


def read_log(log):
    log.flush()
    return Path(log.name).read_text(errors="replace")


def run(args):
    if not os.environ.get("DISPLAY") or not os.environ.get("XDG_RUNTIME_DIR"):
        raise RuntimeError("benchmark needs DISPLAY (X11) and XDG_RUNTIME_DIR")
    if not shutil.which("alacritty"):
        raise RuntimeError("benchmark needs alacritty")
    profile = ["--release"] if args.profile == "release" else []
    subprocess.run(["cargo", "build", "--locked", *profile], cwd=ROOT, check=True)
    if not args.no_bar:
        subprocess.run(["cargo", "build", "--locked", "--manifest-path", str(ROOT / "bar-test/Cargo.toml")], check=True)
    compositor_bin = ROOT / "target" / ("debug" if args.profile == "dev" else "release") / "devel-wm"
    bar_bin = ROOT / "bar-test/target/debug/prototype-bar"
    args.output.parent.mkdir(parents=True, exist_ok=True)
    log_path = args.output.with_suffix(".log")
    processes = []
    with log_path.open("w+") as log:
        try:
            env = os.environ.copy()
            env["DEVEL_WM_PERF"] = "1"
            env.setdefault("RUST_LOG", "error")
            wm = subprocess.Popen([str(compositor_bin)], cwd=ROOT, env=env, stdout=log, stderr=subprocess.STDOUT)
            processes.append(wm)
            wayland = ipc = None
            deadline = time.monotonic() + 15
            while time.monotonic() < deadline and wm.poll() is None:
                text = read_log(log)
                display_match = re.search(r"^WAYLAND_DISPLAY=(\S+)$", text, re.M)
                ipc_match = re.search(r"^DEVEL_WM_IPC=(\S+)$", text, re.M)
                if display_match and ipc_match:
                    wayland, ipc = display_match.group(1), ipc_match.group(1)
                    break
                time.sleep(0.05)
            if not wayland or not ipc:
                raise RuntimeError(f"compositor failed to start; see {log_path}")
            client_env = os.environ.copy()
            client_env.pop("DISPLAY", None)
            client_env.update(WAYLAND_DISPLAY=wayland, DEVEL_WM_IPC=ipc)
            if not args.no_bar:
                bar = subprocess.Popen([str(bar_bin)], env=client_env, stdout=log, stderr=subprocess.STDOUT)
                processes.append(bar)
                time.sleep(0.5)
                if bar.poll() is not None:
                    raise RuntimeError(f"bar failed to start; see {log_path}")
            time.sleep(args.warmup)
            idle_offset = len(read_log(log))
            idle_cpu_start = cpu_seconds(wm.pid)
            idle_start = time.monotonic()
            time.sleep(args.idle)
            idle_elapsed = time.monotonic() - idle_start
            idle_cpu = cpu_seconds(wm.pid) - idle_cpu_start
            idle_text = read_log(log)[idle_offset:]
            workload = (
                "import time,sys; "
                "end=time.monotonic()+float(sys.argv[1]); "
                "i=0; "
                "\nwhile time.monotonic()<end:\n"
                " print(f'{i:06d} '+('x'*74), flush=True); i+=1; time.sleep(1/30)"
            )
            for _ in range(2):
                client = subprocess.Popen(["alacritty", "-e", "python3", "-u", "-c", workload,
                                            str(args.active + args.warmup + 3)],
                                           env=client_env, stdout=log, stderr=subprocess.STDOUT)
                processes.append(client)
            time.sleep(args.warmup)
            if any(process.poll() is not None for process in processes):
                raise RuntimeError(f"a client stopped during warmup; see {log_path}")
            active_offset = len(read_log(log))
            active_cpu_start = cpu_seconds(wm.pid)
            active_start = time.monotonic()
            time.sleep(args.active)
            active_elapsed = time.monotonic() - active_start
            active_cpu = cpu_seconds(wm.pid) - active_cpu_start
            active_text = read_log(log)[active_offset:]
            if wm.poll() is not None:
                raise RuntimeError(f"compositor stopped during benchmark; see {log_path}")
            result = {
                "profile": args.profile,
                "output": args.output.as_posix(),
                "log": log_path.as_posix(),
                "clients": 2,
                "bar": not args.no_bar,
                "idle": {"seconds": idle_elapsed, "cpu_seconds": idle_cpu, "cpu_percent": idle_cpu / idle_elapsed * 100,
                         **metrics(idle_text)},
                "active": {"seconds": active_elapsed, "cpu_seconds": active_cpu, "cpu_percent": active_cpu / active_elapsed * 100,
                           **metrics(active_text)},
            }
            if result["active"]["commits"] == 0 or result["active"]["frames"] == 0:
                raise RuntimeError(f"no active client commits or rendered frames; see {log_path}")
            args.output.write_text(json.dumps(result, indent=2) + "\n")
            print(json.dumps(result, indent=2))
        finally:
            for process in reversed(processes):
                stop(process)


def compare(paths):
    before, after = (json.loads(path.read_text()) for path in paths)
    if (before["profile"], before["bar"], before["clients"]) != (after["profile"], after["bar"], after["clients"]):
        raise RuntimeError("compare runs with the same profile, bar, and client count")
    old_commits, new_commits = before["active"]["commits"], after["active"]["commits"]
    if not old_commits or not new_commits or abs(new_commits / old_commits - 1) > .2:
        print("Warning: active client commit counts differ by over 20%; rendering/CPU comparisons may not be meaningful.", file=sys.stderr)
    print(f"Baseline: {paths[0]}  Candidate: {paths[1]}")
    for phase in ("idle", "active"):
        print(f"{phase} (lower is better except frames):")
        for key in ("cpu_percent", "refresh_us", "cleanup_us", "ipc_us", "render_submit_us_per_frame",
                    "frame_p95_us_max_interval", "redraw_requests", "frames", "damage_fraction"):
            a, b = before[phase].get(key), after[phase].get(key)
            if a is None or b is None:
                continue
            change = f"{(b / a - 1) * 100:+.1f}%" if a else "both zero" if not b else "from zero"
            print(f"  {key:32} {a:12.2f} -> {b:12.2f}  ({change})")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--profile", choices=("dev", "release"), default="dev")
    parser.add_argument("--output", type=Path, default=Path("/tmp/opencode/devel-wm-perf.json"))
    parser.add_argument("--warmup", type=float, default=1.5)
    parser.add_argument("--idle", type=float, default=3)
    parser.add_argument("--active", type=float, default=6)
    parser.add_argument("--no-bar", action="store_true")
    parser.add_argument("--compare", nargs=2, type=Path, metavar=("BASELINE", "CANDIDATE"))
    args = parser.parse_args()
    try:
        compare(args.compare) if args.compare else run(args)
    except (OSError, RuntimeError, subprocess.CalledProcessError) as error:
        print(f"benchmark: {error}", file=sys.stderr)
        sys.exit(1)


if __name__ == "__main__":
    main()
