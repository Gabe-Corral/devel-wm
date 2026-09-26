# devel-wm MVP roadmap

Check an item only after its stated behavior is verified. This tracks the standalone compositor; see [ARCHITECTURE.md](ARCHITECTURE.md) for the design and [README.md](../README.md) for current run instructions. Checked boxes reflect work already demonstrated, not assumptions about unfinished features. The MVP prioritizes a performant, usable desktop over distinctive optional features.

## 0. Baseline and nested bring-up

- [x] Create Cargo project, source boundaries, formatting/Clippy checks, and a local run guide.
- [x] Pin Smithay 0.7.0 and lock Rust dependencies; identify Manjaro X11/i3 as the first development host and Alacritty as a test client.
- [x] Start a nested output, create a Wayland socket, and accept an xdg-shell toplevel.
- [x] Select an EGL/GLES renderer and log the actual GPU (NVIDIA RTX 5070 observed on the development machine).
- [x] Observe an Alacritty client connect and submit buffers and frame requests without the compositor exiting.
- [x] Manually confirm Alacritty is visibly rendered inside the nested output.
- [ ] Manually confirm typing/pointer/scroll input, outer-window resize, and clean client/host-window closure.
- [x] Replace the unconditional redraw loop with change-driven repaint requests; verify an idle output stops repainting and an active Alacritty client still receives frame callbacks.
- [ ] Optimize changed frames with valid buffer-age and partial-damage handling; the nested backend currently performs a full redraw when a repaint is needed.

## 1. Pure window-management core

- [x] Define opaque monitor/window IDs and monitor-local workspace IDs, with one owning monitor and workspace per window.
- [x] Represent an active workspace and focus state independently for every monitor; define the active monitor for keyboard focus in the pure WM core.
- [ ] Add typed commands for window creation/removal, focus, workspace switching, moving to a workspace/monitor, swapping tiled windows, and toggling floating/fullscreen.
- [ ] Maintain invariants under close, transfer, focus changes, and workspace switches; test these transitions without Smithay or a display.
- [ ] Represent output-local logical rectangles and preserve floating geometry across moves and toggles.

## 2. Layout and nested WM behavior

- [ ] Implement deterministic master/stack geometry with configurable master ratio, master count, gaps, and borders; test multiple window counts and small usable areas.
- [ ] Apply WM geometry to real xdg-shell surfaces, including configure/ack/commit behavior; map Smithay surfaces to window IDs.
- [ ] Focus and raise windows correctly from keyboard and pointer input; avoid using Smithay `Space` as the source of WM ordering.
- [ ] Support per-window floating alongside tiled windows; toggling from tiled starts at the on-screen tiled rectangle.
- [ ] Implement Alt + left-button drag: convert tiled to floating at drag start, move with the pointer, and remain floating on release. Test cross-monitor ownership transfers in the pure core with two simulated monitors.
- [ ] Verify with multiple clients that focus, workspace switching, moving, tiling, floating, close, and resize work in the nested backend.

## 3. Configuration and external control

- [ ] Ship usable defaults with **Alt** as modifier, plus documented TOML overrides and duplicate/invalid keybinding detection.
- [ ] Route keyboard actions and local IPC requests through the same typed commands.
- [ ] Expose queries and change events for monitor-local workspaces, focus, windows, and layout; provide a small command client.
- [ ] Limit IPC socket access to the current user and handle normal shutdown/stale sockets.

## 4. Native outputs and multi-monitor behavior

- [ ] Start directly from a Linux session using seat/session management, DRM/KMS, GPU-backed rendering, and libinput; keep the nested backend for development.
- [ ] Give each monitor its own workspaces, active view, focus, output scale, and usable area; test with two physical or virtual outputs.
- [ ] Handle output hotplug, scaling and mode changes, and real cross-output floating drags to the destination's active workspace without losing or duplicating windows.
- [ ] On disconnect, transfer windows to a surviving monitor's active workspace, preserve floating state, clamp positions, and recover focus. If all outputs disappear, retain state until one returns.
- [ ] Check native rendering and input on target hardware and verify the active GPU renderer.
- [ ] Measure idle repaint activity and frame timing during representative terminal/browser and multi-monitor use; address observed regressions before considering optional rendering features.

## 5. MVP desktop usability

- [ ] Support per-output layer-shell bars and reserve their exclusive zones; verify separate bar instances can show each monitor's own IPC state.
- [ ] Provide essential client behavior: cursor images, popups/grabs, clipboard/data-device, and any other protocols needed by the MVP test applications.
- [ ] Provide a working session-lock and unlock path for a native daily-driver session.
- [ ] Complete a manual workflow test: launch terminal and browser, type and point, switch per-monitor workspaces, tile and float, drag between monitors, copy/paste, lock/unlock, unplug/replug an output, and exit cleanly.
- [ ] Run `cargo fmt --all -- --check`, `cargo clippy --all-targets --locked -- -D warnings`, and `cargo test --locked`; keep pure WM/layout tests and real-client smoke tests meaningful.

**MVP complete when:** the standalone compositor runs natively without an X11 host, provides working master/stack and floating windows on independent per-monitor workspaces, supports per-monitor external bars, and passes the workflow above. XWayland compatibility and additional layouts are follow-up decisions/features, not prerequisites silently implied by a successful nested prototype.

## After MVP: optional scripting and other ideas

- [ ] Evaluate embedding [RustPython](https://github.com/RustPython/RustPython) for user automation. Prototype event delivery, typed-command requests, restart/recovery from exceptions and hung callbacks, and the effect on memory and responsiveness before selecting runtime isolation.
- [ ] If the evaluation succeeds, provide an opt-in scripting API for window/workspace/output events and WM commands without replacing TOML defaults or blocking the compositor event loop.
- [ ] Consider Python-defined layouts only after measuring latency and defining validation and Rust-layout fallback behavior.

Detailed inspection, event history, developer sessions, and performance dashboards are also deferred until the core compositor is usable and its rendering behavior has been measured.
