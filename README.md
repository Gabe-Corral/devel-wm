# devel-wm

A standalone Rust Wayland compositor in development. Read the [architecture](docs/ARCHITECTURE.md) for the agreed design, the [MVP roadmap](docs/ROADMAP.md) for checkable work, and the [project plan](docs/PROJECT_PLAN.md) for broader milestones. The current prototype opens a nested X11 window, serving Wayland clients inside it; it is not yet a tiling window manager.

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

It opens a plain nested window. The terminal prints a `WAYLAND_DISPLAY` socket name and the actual GLES vendor, renderer, and version. In a **second** terminal, copy the socket name printed by this run (the example below is not guaranteed to match):

```sh
env -u DISPLAY WAYLAND_DISPLAY=wayland-1 alacritty
```

Removing `DISPLAY` prevents the client from opening an ordinary X11 window instead. Click inside the Alacritty window to give it keyboard focus; type, scroll, and resize the outer X11 window. Close Alacritty normally, then close the outer window to stop the compositor. The server prints `xdg-shell toplevel connected` when the Wayland client attaches. Check that the server log names your GPU, rather than `llvmpipe` or `softpipe`; a visible window by itself does not prove hardware rendering. `RUST_LOG=error cargo run --locked` hides verbose Smithay startup messages while preserving those short diagnostics.

This is an early prototype: windows are stacked at the top-left, there are no keybindings or tiling yet, popup grabs and interactive xdg move/resize are not implemented, and client-rendered cursor images are not drawn. Resizing the outer X11 window sends updated size requests to clients. Some applications require additional Wayland protocols not yet exposed by this prototype.

The nested output requests redraws when clients change or the host window resizes, rather than rendering continuously while idle. Use `RUST_LOG=devel_wm::compositor::nested=debug cargo run --locked` to see `nested repaint` messages while diagnosing redraws. A requested repaint currently redraws the full output; partial-damage rendering is future work.

## Code map

| Module | Responsibility |
| --- | --- |
| `src/compositor` | Smithay protocols, surfaces, outputs, event loop |
| `src/input` | Keyboard and pointer events |
| `src/wm` | Window/workspace state, focus, commands |
| `src/layout` | Pure layout calculations |
| `src/render` | GPU rendering and frame scheduling |
| `src/config` | Defaults, TOML loading and validation |
| `src/ipc` | Local socket, commands and queries |

The `wm` module contains Smithay-independent window ownership, per-monitor active workspaces, and per-workspace focus. The nested compositor registers its output and windows in this state, routes click focus through it, and removes windows when clients close. Keyboard focus derives from the active monitor's visible workspace. Workspace switching and multi-output display behavior are not wired to rendering or shortcuts yet. The `layout`, `render`, `config`, and `ipc` modules remain reserved for their planned responsibilities. The current nested rendering code lives in `src/compositor/nested.rs`; Smithay protocol handlers live in `src/compositor/handlers.rs` and input forwarding in `src/input/mod.rs`.
