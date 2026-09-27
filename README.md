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

To start the compositor **and the prototype test bar** together instead, run `scripts/run-with-bar.sh` from any directory. The script builds both packages, passes the compositor's current Wayland and IPC socket names to the bar, and stops them when you press Ctrl-C or close the nested window. Use `scripts/run-with-bar.sh --alacritty 2` to also open two Alacritty windows on that compositor (and stop them on exit). `--alacritty` accepts 0–32; without it, no terminals start. You can also pass `--config /path/to/config.toml` to the compositor, in either order with `--alacritty`.

### Keybindings

Bindings use built-in defaults unless overridden in `$XDG_CONFIG_HOME/devel-wm/config.toml` (or `~/.config/devel-wm/config.toml` when `XDG_CONFIG_HOME` is unset). You can also choose a file explicitly with `cargo run --locked -- --config /path/to/config.toml`. A missing default file is fine; a missing explicit file, invalid key, unknown action, or duplicate shortcut stops startup with an error. Restart to apply edits; live reload is not implemented.

For example, this replaces selected defaults while keeping all other bindings:

```toml
[keybindings]
focus-next = "Super+h"
focus-previous = "Super+l"
switch-workspace-1 = "Super+1"
move-focused-to-workspace-1 = "Super+Shift+1"
toggle-floating = "Super+space"
```

Names: `switch-workspace-N` and `move-focused-to-workspace-N` (positive workspace numbers), `focus-next`, `focus-previous`, `swap-next`, `swap-previous`, `promote-master`, `shrink-master`, `grow-master`, and `toggle-floating`. Chords use `Alt`, `Ctrl`, `Shift`, and `Super` (or `Logo`) with one XKB key name such as `j`, `Return`, `space`, `Left`, or `F12`; modifier order and spelling case do not matter. The existing defaults are Alt+1–9, Alt+Shift+1–9, Alt+j/k, Alt+Shift+j/k, Alt+Return, Alt+h/l, and Alt+Space. **Alt+left-drag and Alt+right-drag are pointer bindings and remain hard-coded** for now. Your X11 host WM may still intercept a shortcut before the nested compositor receives it.

It opens a plain nested window. The terminal prints a `WAYLAND_DISPLAY` socket name and the actual GLES vendor, renderer, and version. In a **second** terminal, copy the socket name printed by this run (the example below is not guaranteed to match):

```sh
env -u DISPLAY WAYLAND_DISPLAY=wayland-1 alacritty
```

Removing `DISPLAY` prevents the client from opening an ordinary X11 window instead. Launch a **second Alacritty with the same socket**: they tile as master and stack, rather than overlap (confirmed in the nested output). New windows take focus on their visible workspace. **Alt+j/k** cycles focus through windows, **Alt+Shift+j/k** swaps the focused tile with its neighbor, and **Alt+Return** promotes it to master. Close either window, resize the outer X11 window, and check that the remaining clients get new sizes. Focus/order behavior was manually confirmed in the nested output. With the nested output focused, use **Alt+1** through **Alt+9** to switch its numbered workspaces. **Alt+Shift+1** through **Alt+Shift+9** moves the focused window to that workspace *without switching to it*; use Alt+number afterward to see it there. Workspace switching and moving a focused window were manually confirmed in the nested output. Returning to a workspace restores its remembered keyboard focus. The X11 host WM may intercept a binding before it reaches the nested window. Close Alacritty normally, then close the outer window to stop the compositor. The server prints `xdg-shell toplevel connected` when the Wayland client attaches. Check that the server log names your GPU, rather than `llvmpipe` or `softpipe`; a visible window by itself does not prove hardware rendering. `RUST_LOG=error cargo run --locked` hides verbose Smithay startup messages while preserving those short diagnostics.

This is an early prototype: **Alt+Space** toggles the focused window between tiled and floating. **Alt+left-drag** converts a tiled window to floating at its current on-screen size and position, preserving the pointer's grip offset while moving it; releasing the button leaves it floating. **Alt+right-drag** resizes an already-floating window from the nearest corner, anchoring the opposite corner and clamping to at least 120×80 logical pixels; it does not resize tiled windows. **Alt+h/l** shrinks/grows the master column by 5 percentage points on the active workspace, bounded to 20–80%; the ratio is retained separately for each monitor-local workspace and does nothing when fewer than two windows are tiled. A large window can extend partly outside the owning output, but a grabbable strip remains visible. Floating windows retain their position when moved between workspaces or toggled back to floating. Moving a floating window between outputs and interactive client-requested xdg move/resize are not yet supported. Clients that negotiate server-side `xdg-decoration` receive a 24-pixel titlebar and 2-pixel border, with bitmap title text and focus colors; client-side-decorated windows keep their own decorations. Clicking the server-drawn titlebar focuses the window; left-dragging it by at least four pixels starts the same floating move as Alt+left-drag, without requiring Alt. A click without a drag leaves a tiled window tiled. The decoration is included in the tiled and floating size and hit region. This has been exercised with Alacritty's server-side negotiation, but the client-side fallback and new resize/pointer behavior still need manual verification. Popup grabs are not implemented. The nested output draws a built-in pointer and client-supplied cursor surfaces (including their hotspot and hidden state); named cursor shapes currently fall back to the built-in arrow. Resizing the outer X11 window sends updated size requests to tiled clients. Some applications require additional Wayland protocols not yet exposed by this prototype.

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
