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

- [x] Implement deterministic master/stack geometry from an output-local logical usable area, ordered tiled window IDs, and validated settings to window-ID/rectangle placements. Keep Smithay and scripting out of the calculation; test multiple window counts, small areas, invalid settings, and non-overlap.
- [x] Apply WM geometry to real xdg-shell surfaces, including configure/ack/commit behavior; map Smithay surfaces to window IDs. Two Alacritty windows were confirmed tiling in the nested output.
- [x] Focus and raise windows correctly from keyboard and pointer input; avoid using Smithay `Space` as the source of WM ordering. The focus/order workflow was manually confirmed in the nested output.
- [x] Support per-window floating alongside tiled windows; toggling from tiled starts at the on-screen tiled rectangle. Floating and pointer-anchored dragging were manually confirmed in the nested output.
- [ ] Implement Alt + left-button drag: convert tiled to floating at drag start, move with the pointer, and remain floating on release. Test cross-monitor ownership transfers in the pure core with two simulated monitors.
- [ ] Negotiate server-side decorations with clients that support `xdg-decoration` so they omit their own titlebars. Handle clients that insist on client-side decorations without drawing a duplicate titlebar; verify behavior with both kinds of client.
- [ ] Draw a simple i3-style top titlebar and border for each server-decorated window, showing the current xdg-toplevel title. Update on title and focus changes; include decoration height in layout, hit testing, and configure sizes, and omit the titlebar in fullscreen.
- [ ] Verify with multiple clients that focus, workspace switching, moving, tiling, floating, close, and resize work in the nested backend.

## 3. Configuration and external control

- [ ] Ship usable defaults with **Alt** as modifier, plus documented TOML overrides and duplicate/invalid keybinding detection.
- [ ] Provide a documented, validated TOML color scheme for focused/unfocused titlebars, borders, and title text, with usable defaults. Verify configured colors render correctly and invalid colors produce clear errors.
- [ ] Route keyboard actions and local IPC requests through the same typed, validated commands; return explicit errors for invalid targets. Keep this path usable without scripting.
- [ ] Expose queries and change events for monitor-local workspaces, focus, windows, and layout; provide a small command client.
- [ ] Limit IPC socket access to the current user and handle normal shutdown/stale sockets.
- [ ] Before enabling write-capable IPC, authenticate local peers and authorize the narrow window/workspace commands exposed to them; test unauthorized requests. A private socket alone is not sufficient authorization for desktop control.

## 4. Native outputs and multi-monitor behavior

- [ ] Start directly from a Linux session using seat/session management, DRM/KMS, GPU-backed rendering, and libinput; keep the nested backend for development.
- [ ] Give each monitor its own workspaces, active view, focus, output scale, and usable area; test with two physical or virtual outputs.
- [ ] Handle output hotplug, scaling and mode changes, and real cross-output floating drags to the destination's active workspace without losing or duplicating windows.
- [ ] On disconnect, transfer windows to a surviving monitor's active workspace, preserve floating state, clamp positions, and recover focus. If all outputs disappear, retain state until one returns.
- [ ] Check native rendering and input on target hardware and verify the active GPU renderer.
- [ ] Measure idle repaint activity and frame timing during representative terminal/browser and multi-monitor use; address observed regressions before considering optional rendering features.

## 5. MVP desktop usability

- [ ] Support per-output layer-shell bars and reserve their exclusive zones; verify separate bar instances can show each monitor's own IPC state.
- [x] Render a default cursor and client-provided cursor surfaces with hotspot in the nested output; verify the custom cursor from `bar-test` appears and restores on pointer leave.
- [ ] Provide the remaining essential client behavior: popups/grabs, clipboard/data-device, and any other protocols needed by the MVP test applications. The client-owned clipboard and explicit xdg popup grabs are wired up; verify the [clipboard](CLIPBOARD.md) and [popup](POPUPS.md) smoke tests before marking this complete. Verify cursor behavior on the native backend separately.
- [ ] Provide a working session-lock and unlock path for a native daily-driver session.
- [ ] Complete a manual workflow test: launch terminal and browser, type and point, switch per-monitor workspaces, tile and float, drag between monitors, copy/paste, lock/unlock, unplug/replug an output, and exit cleanly.
- [ ] Run `cargo fmt --all -- --check`, `cargo clippy --all-targets --locked -- -D warnings`, and `cargo test --locked`; keep pure WM/layout tests and real-client smoke tests meaningful.

Decoration negotiation cannot forcibly remove titlebars drawn into a client's own buffer. The target is one compositor-drawn titlebar where the client supports server-side decorations, with a deliberate fallback for clients that do not.

**MVP complete when:** the standalone compositor runs natively without an X11 host, provides working master/stack and floating windows on independent per-monitor workspaces, supports per-monitor external bars, and passes the workflow above. XWayland compatibility and additional layouts are follow-up decisions/features, not prerequisites silently implied by a successful nested prototype.

## After MVP: secure automation and optional scripting

The proposed differentiator is **an inspectable, explainable, programmable developer desktop with a secure, language-neutral control plane**—not merely Python instead of Lua or an agent that guesses where to click. Human shortcuts, a CLI, Python, and AI agents should use the same validated policy. Keep the MVP usable without Python or an agent; complete the native desktop and measure its baseline before adding a runtime. The current read-only IPC is a starting point, not an authorized desktop-control API.

### A. Establish a language-neutral automation boundary

- [ ] Define versioned, documented state snapshots, queries, typed commands/results, and structured ordered events (with sequence numbers and stable process-lifetime IDs). Verify ordering, reconnect/resync, validation errors, and behavior when subscribers are slow. The current read-only IPC snapshots are a starting point, not the finished event API.
- [ ] Make a CLI, Python wrapper/REPL, and an agent adapter clients of the **same** control interface; none may mutate Smithay objects or bypass Rust WM policy. Implement semantic operations such as selecting/focusing/closing a window or switching a workspace before resorting to synthetic keystrokes or coordinates. Resolve ambiguous app-id/title selectors explicitly; prefer stable window IDs for follow-up commands.
- [ ] Support bounded event subscriptions and wait-for conditions (for example, launch an application, await its window-created event with a timeout, then act on its ID) without polling or blocking input/rendering. Test event loss, slow subscribers, disconnection, and resync.
- [ ] Provide optional process metadata (PID, executable, cwd, ancestry/cgroup where available) with provenance, missing-data/error states, and access controls. Test PID reuse, shared clients, short-lived processes, and windows whose creator is not their actual application; never infer window ownership or placement solely from a PID or cwd.
- [ ] Record a bounded, privacy-conscious explanation of important policy decisions (for example, which rule placed a window), referencing the triggering event and applied command. Expose it for inspection without putting logging or external subscribers on the frame/input critical path.

### B. Secure the control plane before expanding its powers

- [ ] Threat-model untrusted local processes, compromised scripts, and agents. Authenticate each connection, default-deny sensitive operations, and enforce revocable capabilities for window/workspace read and control, process metadata, clipboard read/write, screenshot capture, keyboard/pointer injection, and application launch. Test cross-client data leaks, bypasses, stale/reused identities, revocation, and denial of service.
- [ ] Scope sensitive access to explicit windows/workspaces/outputs where feasible, with user-visible consent and auditable decisions. Treat a workspace as a **policy scope**, not a proven security isolation boundary; verify what a client can actually observe before promising isolation. Do not expose an unrestricted Python interpreter or compositor internals over IPC.
- [ ] Add screenshot, clipboard, application-launch, and synthetic-input APIs only as separately authorized capabilities after the underlying desktop features work. Prefer semantic WM operations; screenshots and input are fallbacks because the compositor cannot inherently see buttons or fields inside applications. Explore accessibility or application-specific semantics independently, without assuming Wayland surface metadata provides them.

### C. Prototype safe Python automation

- [ ] Compare [RustPython](https://github.com/RustPython/RustPython), a separately restartable Python process, and other practical runtimes against compatibility, memory, startup time, event latency, and failure isolation. Do not choose embedding solely because it is possible.
- [ ] Prototype an opt-in Python client of the control plane that reads snapshots/events and submits validated commands; prohibit direct access to Smithay objects or mutable WM internals. Force exceptions, infinite loops, slow handlers, and process crashes; demonstrate that input, rendering, built-in layouts, TOML defaults, and IPC remain usable. Embedding RustPython is optional, not the control API.
- [ ] Add safe script reload: validate and register new handlers before replacing old ones, retain the previous working automation on load failure, and report actionable errors. Do not promise rollback of commands already executed by an old script.

### D. Prove useful developer and agent workflows

- [ ] Build one end-to-end **open project** workflow: launch a terminal/editor for a project, associate their windows with a workspace using evidence that can be inspected or corrected, and manipulate the live result through a Python REPL. Explain why each window was assigned; validate behavior when process metadata is missing or wrong.
- [ ] Evaluate save/reopen only as **relaunching applications and reconstructing intent**, not serializing/restoring Wayland surfaces. Test application failures, duplicate windows, and user overrides before considering general project/session management.
- [ ] Measure overhead and recovery versus the no-Python baseline in terminal/browser use. Treat the feature as ready only if it delivers the workflow without stalls, lost commands, or a broken desktop after script failure; otherwise keep it experimental and opt-in.
- [ ] Demonstrate an agent using least-privilege, semantic commands and events to launch an application, await an unambiguous window, focus/move it, and recover from denial, timeout, and disconnect. Compare reliability against screenshot/coordinate-driven control without treating app internals as compositor-known state.
- [ ] Explore dedicated agent workspaces or virtual outputs only after authorization, screenshot/input scoping, and isolation have been demonstrated. Test interference with human focus, clipboard, other workspaces, and physical outputs; do not make isolated agent sessions an MVP prerequisite.

Python-defined layouts, detailed rendering diagnostics, richer inspection dashboards, and broader project APIs are later experiments: first validate results, deadlines, and a deterministic Rust-layout fallback. Do not make them prerequisites for master/stack or daily-driver usability.
