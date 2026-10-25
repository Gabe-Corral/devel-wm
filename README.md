# devel-wm

A standalone Rust Wayland compositor in development. Read the [architecture](docs/ARCHITECTURE.md) for the agreed design, the [MVP roadmap](docs/ROADMAP.md) for checkable work, and the [project plan](docs/PROJECT_PLAN.md) for broader milestones. The current prototype opens a nested X11 window, serving Wayland clients inside it with initial master/stack tiling; it is not yet a complete window manager.

## Local setup

Install Rust with Cargo, rustfmt, and Clippy, plus the X11, Wayland, EGL/GLES, and xkbcommon runtime/development libraries. On Manjaro, the host uses an X11/i3 session; the nested backend uses Smithay **0.7.0** (`backend_winit`, `desktop`, `wayland_frontend`), with its complete dependency versions recorded in `Cargo.lock`. For the tested machine the relevant system libraries are provided by Manjaro's `libglvnd`, `wayland`, `libxkbcommon`, and NVIDIA driver packages; `alacritty` is the test client.

From the repository root:

```sh
cargo build
cargo fmt --all -- --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked
```

## Run nested on X11/i3

In your X11 session, open a terminal and start the compositor:

```sh
cargo run --locked
```

`cargo run --locked -- --backend nested` selects the same mode explicitly.
`--backend native` without the `native` Cargo feature exits with an error.
`--backend` and `--config` can be passed in either order; `--help` lists usage.

### Native device probe

Milestone 2 only enumerates DRM cards/connectors through udev and opens card
devices through libseat to query their current state. It does **not** modeset,
render, start input, serve Wayland clients, or open an output. It refuses a
terminal emulator or SSH session: **use a separate Linux text login/VT**, not
the active X11/i3 display. Install the libseat runtime and development package
first (including its pkg-config file), then build in your existing session:

```sh
cargo build --locked --features native
```

Switch to a separate text VT, log in, and run the built binary from this project
directory (without `sudo`):

```sh
./target/debug/devel-wm --backend native
```

It prints the seat, device paths/drivers, connector states and mode counts,
then closes the devices and exits. An inactive seat, inaccessible device, or
failed query is reported as an error. Switch back to your graphical VT after it
exits. Do not run the probe from an X11/i3 terminal; the VT restriction is
intentional. Nested mode remains the default, even in a native-feature build.

### Single-output native presentation test

The opt-in presentation test uses DRM/KMS, GBM, EGL, and GLES to show a blue
background with a green center rectangle on **one** connected output. It logs
the native GPU renderer, waits for page-flip completion, holds the frame for
five seconds, and attempts to restore the previous display and console state.
It does not start Wayland clients or libinput. `--backend native` alone still
runs the discovery-only probe.

Build with `cargo build --locked --features native`, then **from a separate
text login/VT**, run:

```sh
./target/debug/devel-wm --backend native --native-present HDMI-A-2 > native-normal.log 2>&1
echo $?
cat native-normal.log
```

Save work in i3 first. Ctrl+C on the graphics VT may not generate SIGINT (the
presentation probe has no input); the five-second timeout still ends the test.
To verify signal cleanup, use the timed-SIGINT recipe in [docs/NATIVE.md](docs/NATIVE.md).
Switching VTs ends the test instead of resuming it. No fallback connector is selected if HDMI-A-2 is not
connected. Scoped restoration passed the normal-exit and timed-SIGINT NVIDIA
hardware tests; controlled presentation VT-switch cleanup also passed. Native
the bounded native client switch and resume round trip also passed; display/console/device-close
failures cause a nonzero exit status. See
[the native smoke-test guide](docs/NATIVE.md) for expectations and recovery.
The guide also covers an opt-in `--native-switch-vt N` presentation probe. It asks libseat to switch to a known
graphical VT; keyboard VT switching is not available in the presentation probe.

### Experimental native client

From a separate Linux text VT, `--backend native --native-session` explicitly
starts an unlimited native session on automatically discovered outputs of one
DRM card. A named connector (`HDMI-A-2`) or pair (`HDMI-A-2,DP-4`) can instead
be supplied for diagnosis. Build with `cargo build --locked --features native`;
then use Alt+Shift+Return to launch a terminal and Ctrl+Alt+Backspace to exit
cleanly. The session starts without clients or a timeout. The default backend
remains nested; `--backend native` alone is a read-only discovery probe. Save
your graphical-session work and follow [docs/NATIVE.md](docs/NATIVE.md) for
text-VT and recovery precautions before running native.

To start the native session with one prototype bar, run
`scripts/run-native-with-bar.sh` from a separate Linux text VT. The bar appears
on the compositor's default output, not every monitor. The launcher accepts
`--config PATH`; it has no bounded mode or automatic shutdown.

To start the compositor **and the prototype test bar** together instead, run `scripts/run-with-bar.sh` from any directory. The script builds both packages, passes the compositor's current Wayland and IPC socket names to the bar, and stops them when you press Ctrl-C or close the nested window. Use `scripts/run-with-bar.sh --alacritty 2` to also open two Alacritty windows on that compositor (and stop them on exit). `--alacritty` accepts 0–32; without it, no terminals start. You can also pass `--config /path/to/config.toml` to the compositor, in either order with `--alacritty`.

### Keybindings

Bindings use built-in defaults unless overridden in `$XDG_CONFIG_HOME/devel-wm/config.toml` (or `~/.config/devel-wm/config.toml` when `XDG_CONFIG_HOME` is unset). You can also choose a file explicitly with `cargo run --locked -- --config /path/to/config.toml`. A missing default file is fine; a missing explicit file, invalid key, unknown action, or duplicate shortcut stops startup with an error. Restart to apply edits; live reload is not implemented.

The explicit native client/session additionally accepts `[outputs.HDMI-A-2]`
and `[outputs.DP-4]` settings with `mode = "WIDTHxHEIGHT@HZ"` (advertised mode)
and integer `scale = 1`–`4`. Unspecified outputs retain their preferred mode
and scale 1; nested behavior is unchanged. Startup-only scale/mode selection
has been checked at HDMI-A-2 scale 2; selecting a different advertised timing
still needs a hardware check. See [docs/NATIVE.md](docs/NATIVE.md) for bounded
native tests that do not overwrite your normal config.

Tiled window spacing can be set independently in the same file:

```toml
[layout]
inner_gap = 12  # between tiled windows, in logical pixels
outer_gap = 20  # between tiled windows and usable screen edges, in logical pixels
```

Both default to 8; zero removes a gap, and negative or non-integer values are
rejected. At native output scale 2, one logical pixel occupies two physical
pixels. With both gaps at zero, tiled frames fill their entire allocation;
server-decorated windows still draw their 2-pixel border within it. Gaps apply to
tiled windows on both nested and native outputs, respect bar-exclusive usable
areas, and do not reposition floating windows. Changes
take effect on restart, not live reload.

For example, this replaces selected defaults while keeping all other bindings:

```toml
[keybindings]
focus-next = "Super+h"
focus-previous = "Super+l"
switch-workspace-1 = "Super+1"
move-focused-to-workspace-1 = "Super+Shift+1"
toggle-floating = "Super+space"
launch-terminal = "Super+t"
close-focused = "Super+q"
toggle-tiling-mode = "Super+t"

[terminal]
command = ["foot", "--title", "dev"]
```

Names: `switch-workspace-N` and `move-focused-to-workspace-N` (positive workspace numbers), `focus-next`, `focus-previous`, `swap-next`, `swap-previous`, `promote-master`, `shrink-master`, `grow-master`, `toggle-floating`, `launch-terminal`, `close-focused`, and `toggle-tiling-mode`. Chords use `Alt`, `Ctrl`, `Shift`, and `Super` (or `Logo`) with one XKB key name such as `j`, `Return`, `space`, `Left`, or `F12`; modifier order and spelling case do not matter. The existing defaults include Alt+1–9, Alt+Shift+1–9, Alt+j/k, Alt+Shift+j/k, Alt+Return, Alt+h/l, Alt+Space, and **Alt+Shift+Return** to open a terminal, and **Alt+Ctrl+q** to request that the focused window close (clients may prompt to save work), and **Alt+t** to toggle the active workspace between master–stack and equal-width side-by-side columns. `[terminal].command` is an argument array, not a shell command; the default is `["alacritty"]`. The compositor supplies its own `WAYLAND_DISPLAY` and removes `DISPLAY` for launched terminals. A failed launch is logged without ending the session. **Alt+left-drag and Alt+right-drag are pointer bindings and remain hard-coded** for now. Your X11 host WM may still intercept a shortcut before the nested compositor receives it.

It opens a plain nested window. The terminal prints a `WAYLAND_DISPLAY` socket name and the actual GLES vendor, renderer, and version. In a **second** terminal, copy the socket name printed by this run (the example below is not guaranteed to match):

```sh
env -u DISPLAY WAYLAND_DISPLAY=wayland-1 alacritty
```

Removing `DISPLAY` prevents the client from opening an ordinary X11 window instead. Launch a **second Alacritty with the same socket**: they tile as master and stack, rather than overlap (confirmed in the nested output). New windows take focus on their visible workspace. **Alt+j/k** cycles focus through windows, **Alt+Shift+j/k** swaps the focused tile with its neighbor, and **Alt+Return** promotes it to master. Close either window, resize the outer X11 window, and check that the remaining clients get new sizes. Focus/order behavior was manually confirmed in the nested output. With the nested output focused, use **Alt+1** through **Alt+9** to switch its numbered workspaces. **Alt+Shift+1** through **Alt+Shift+9** moves the focused window to that workspace *without switching to it*; use Alt+number afterward to see it there. Workspace switching and moving a focused window were manually confirmed in the nested output. Returning to a workspace restores its remembered keyboard focus. The X11 host WM may intercept a binding before it reaches the nested window. Close Alacritty normally, then close the outer window to stop the compositor. The server prints `xdg-shell toplevel connected` when the Wayland client attaches. Check that the server log names your GPU, rather than `llvmpipe` or `softpipe`; a visible window by itself does not prove hardware rendering. `RUST_LOG=error cargo run --locked` hides verbose Smithay startup messages while preserving those short diagnostics.

This is an early prototype: **Alt+Space** toggles the focused window between tiled and floating. **Alt+left-drag** converts a tiled window to floating at its current on-screen size and position, preserving the pointer's grip offset while moving it; releasing the button leaves it floating. **Alt+right-drag** resizes an already-floating window from the nearest corner, anchoring the opposite corner and clamping to at least 120×80 logical pixels; it does not resize tiled windows. **Alt+h/l** shrinks/grows the master column by 5 percentage points on the active workspace, bounded to 20–80%; the ratio is retained separately for each monitor-local workspace and does nothing when fewer than two windows are tiled. A large window can extend partly outside the owning output, but a grabbable strip remains visible. Floating windows retain their position when moved between workspaces or toggled back to floating. Cross-output floating drags and client-requested xdg moves are implemented but still need hardware verification; client-requested xdg resize is not yet supported. Clients that negotiate server-side `xdg-decoration` receive a 24-pixel titlebar enclosed by a continuous 2-pixel border, with scale-aware anti-aliased title text and focus colors; client-side-decorated windows keep their own decorations. Clicking the server-drawn titlebar focuses the window; left-dragging it by at least four pixels starts the same floating move as Alt+left-drag, without requiring Alt. A click without a drag leaves a tiled window tiled. The decoration is included in the tiled and floating size and hit region. Side-by-side mode keeps the same inner/outer gaps, focus and tile order; floating windows stay floating, and each monitor-local workspace remembers its own mode and master ratio. In side-by-side mode, Alt+h/l shrinks/grows the focused column against its neighbor without changing the saved master ratio. Left-drag a shared vertical edge (within four logical pixels, below the titlebars) to resize the two adjacent columns; the pointer switches to the themed horizontal-resize icon on hover and during the drag; widths follow their windows and adapt to output resizing. Columns clamp to at least 80 logical pixels when space permits. For now, titlebar dragging still starts a floating move; tile-reordering by titlebar drag is planned separately, while Alt+left-drag will remain the floating gesture. This has been exercised with Alacritty's server-side negotiation, but the client-side fallback and new resize/pointer behavior still need manual verification. Explicit xdg popup grabs and output-constrained popup positioning are wired up, but need manual protocol verification; see [popup testing](docs/POPUPS.md). The nested and native outputs draw the system's Xcursor theme for compositor-owned and named cursors, preserving each image's hotspot and choosing the nearest available resolution for the output scale. Client-supplied cursor surfaces and the hidden state are unchanged. `XCURSOR_THEME` and `XCURSOR_SIZE` can override the theme and logical size at startup; if the desktop default is unavailable, the installed `breeze-cursors` theme is tried, then the built-in arrow. Resizing the outer X11 window sends updated size requests to tiled clients. Some applications require additional Wayland protocols not yet exposed by this prototype.

The compositor now exposes `wlr-layer-shell` for external bars: it arranges layer surfaces, renders them above or below normal windows, forwards pointer events and frame callbacks, and applies exclusive zones to the prototype's window area. The standalone [prototype test bar](bar-test/README.md) was manually checked on the nested output: it appears, changes color on click, follows outer-window resizing, reserves space above Alacritty, and releases that space when closed. The [read-only IPC state feed](docs/IPC.md) also lets the prototype bar display workspace state; the bar was manually confirmed working against the nested compositor. No production bar is bundled or started automatically, and bar clicks cannot issue commands yet. The nested backend currently has one output; separate bars per output must be verified when multi-output support is available.

The nested output requests redraws when clients change or the host window resizes, rather than rendering continuously while idle. Use `RUST_LOG=devel_wm::compositor::nested=debug cargo run --locked` to see `nested repaint` messages while diagnosing redraws. A requested repaint currently redraws the full output; partial-damage rendering is future work.

Floating windows are stacked above tiled master/stack windows, even when a tiled window is opened or focused later. To check this in the nested output, float one Alacritty with Alt+Space, then open and focus another tiled window; the float should remain visible and clickable above it. Focus a second overlapping float to check ordering within the floating group, then tile the first float again to check that it returns below remaining floats. This stacking behavior has not yet been manually verified on the nested backend.

Use the [performance measurement guide](docs/PERFORMANCE.md) for opt-in timings, a reproducible nested workload, and explicit limitations of the current full-damage renderer.

## Code map

| Module | Responsibility |
| --- | --- |
| `src/compositor` | Smithay protocols, surfaces, outputs, event loop |
| `src/input` | Keyboard and pointer events |
| `src/wm` | Window/workspace state, focus, commands |
| `src/layout` | Pure layout calculations |
| `src/render` | GPU rendering and frame scheduling |
| `src/config` | Defaults, TOML loading and validation |
| `src/ipc` | Read-only local socket state queries/subscriptions (commands planned) |

The `wm` module contains Smithay-independent window ownership, per-monitor active workspaces, per-workspace tile order/focus, and typed switch/move/focus/swap/promote commands. The nested compositor registers its output and windows in this state, routes click focus through it, and removes windows when clients close, including hidden windows. Keyboard focus derives from the active monitor's visible workspace. Command results reconcile which windows are mapped into Smithay `Space`; its stacking order does not determine tile positions. Multi-output display behavior is still unverified. `src/layout/mod.rs` calculates pure, deterministic master/stack frame and client rectangles; the compositor maps visible windows to the client rectangles and sends size configures when geometry changes. `src/config/mod.rs` loads and validates keyboard shortcuts before startup; `src/ipc/mod.rs` provides the read-only state feed. The `render` module remains reserved for its planned responsibilities. The current nested rendering code lives in `src/compositor/nested.rs`; Smithay protocol handlers live in `src/compositor/handlers.rs` and input forwarding in `src/input/mod.rs`.
