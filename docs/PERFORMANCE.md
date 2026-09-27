# Performance measurement

Performance is a requirement, not a conclusion from passing tests. The nested
backend can be measured on an X11 host with Alacritty installed:

```sh
python3 scripts/bench-nested.py --output /tmp/opencode/wm-before.json
# Make a change, then run the same command with the same display size/profile:
python3 scripts/bench-nested.py --output /tmp/opencode/wm-after.json
python3 scripts/bench-nested.py --compare /tmp/opencode/wm-before.json /tmp/opencode/wm-after.json
```

The benchmark builds the compositor and prototype bar, runs a three-second idle
period followed by two Alacritty clients updating at 30 lines/second for six
seconds, and shuts everything down afterward. It stores JSON results and a
neighboring `.log`. Keep output resolution, GPU, host compositor, profile,
workload and bar setting identical; repeat runs to assess variance. Use
`--profile release` for representative optimized builds (both runs must use
the same profile), and `--no-bar` to isolate compositor/client behavior.
This is not a headless CI benchmark: it opens a nested X11 window on your
desktop. It does not simulate mouse movement or workspace switching.

To inspect a live compositor without running the benchmark, start it with
`DEVEL_WM_PERF=1 cargo run --locked`. `PERF` lines report event/commit counts,
refresh, cleanup, IPC, bind+render, and submit wall-clock microseconds,
redraws, submitted frames, damage pixels and the maximum of each one-second
interval's frame-time p95. Instrumentation is opt-in; timings are **not GPU
hardware timestamps**. An idle event loop may emit no interval line at all;
the benchmark samples compositor CPU time from `/proc/PID/stat` separately.
Do not interpret fewer frames or callbacks as a speedup if the work changed.

For IPC snapshot construction alone, run:

```sh
cargo test --release --locked snapshot_scaling_benchmark -- --ignored --nocapture
```

The ignored benchmark constructs 2,048 windows over 256 workspaces and times
the old per-workspace full scan versus the workspace-owned order using the
same state. It is a scalability comparison, not representative desktop use.

## Current results and limit

Initial five-second **debug-profile** runs on the development machine with a
1913×2092 nested output, test bar, and two active Alacritty clients:

| Metric | Before cleanup/snapshot/arrange changes | After | Interpretation |
| --- | ---: | ---: | --- |
| Compositor idle CPU (3 s) | 0.0 s | 0.0 s | No idle regression observed. |
| Active CPU (5 s) | 0.48 s | 0.39 s | Single-run result; too noisy to establish a general speedup. |
| Dead-window cleanup | 1,757 µs | 4 µs | Event-driven teardown avoids routine registry scans. |
| Refresh | 32,307 µs / 1,343 callbacks | 15,658 µs / 556 callbacks | Callback counts differ; no per-refresh gain established. |
| Bind+render+submit / frame | 918 µs | 980 µs | No rendering gain established. |
| Submitted frames | 299 | 282 | Not a performance improvement by itself. |
| Damaged output pixels | 100% | 100% | Still full-output damage on every submitted frame. |

The snapshot scalability comparison in the debug profile took 1,914,869 µs
with the old scan and 16,809 µs with the workspace-owned order (100 snapshots;
~114× faster in this deliberately large synthetic case). Normal IPC snapshots
are small, and the active-client benchmark recorded ~1 µs total IPC time.
In a separate optimized test run, the same 100 snapshots took 43,549 µs
versus 959 µs (~45×); this is not a compositor frame-time comparison.

The first titlebar-enabled debug run (two Alacritty clients) submitted 300
frames in five seconds, at ~787 µs bind/render/submit per frame and 0.39 s
compositor CPU. It had **300** active client commits versus **1,806** in the
earlier undecorated run; these are different effective workloads, so the
numbers do **not** establish a rendering speedup. The benchmark now rejects
zero-activity runs and warns on large commit-count differences.

Full-output redraw is **not solved**. Smithay 0.7's winit backend exposes
`buffer_age()` only after `bind()`, but `bind()` returns a framebuffer that
borrows the backend, preventing a safe age query before rendering. Guessing
an age risks stale pixels. The nested backend therefore continues to use age
zero. A safe upstream API returning age with the framebuffer (or equivalent
backend change), followed by correctness checks and matching before/after
measurements, is needed before claiming partial-damage improvements.
