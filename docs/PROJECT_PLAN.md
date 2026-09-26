# devel-wm project plan

The [architecture](ARCHITECTURE.md) records the agreed behavior; the [MVP roadmap](ROADMAP.md) is the checkable implementation tracker. This file describes the broader development milestones.

## Goal

Build a standalone, keyboard-first, GPU-accelerated Wayland tiling compositor in Rust, with dwm's small layout model and i3's practical configuration and command interface. Develop it nested first, then support running directly on Linux hardware. The release target is a usable native Wayland daily driver.

## Decisions

| Area | v1 decision |
| --- | --- |
| Compositor foundation | Smithay; use its Anvil example as reference material |
| Development path | Nested compositor first, then native DRM/KMS and input |
| Window organization | Independent numbered workspaces per monitor; each window belongs to one monitor and one workspace |
| Tiling | Master/stack and per-window floating first; additional layouts later |
| Rendering | GPU-backed composition without animations or visual effects |
| Customization | Alt as configurable default modifier; usable built-in defaults, TOML configuration, local IPC |
| Release target | Daily-driver quality, including multi-monitor support and essential desktop protocols |

The first development target is Manjaro Linux on X11/i3 using the Smithay 0.7.0 winit backend; Alacritty is the initial smoke-test client. The nested EGL/GLES path was observed selecting an NVIDIA GPU. See [README.md](../README.md) for local launch and test commands.

## Architecture

- **Compositor integration:** Smithay owns Wayland protocol objects, outputs, input, rendering, frame scheduling, and surface lifecycle. Keep an explicit mapping between Smithay surfaces and internal window IDs.
- **Window-management core:** Store monitor-local workspace membership, focus order, tiling order, floating/fullscreen state, and commands independently of Smithay where practical. Treat Wayland focus and surface state as integration concerns, not as purely abstract data.
- **Layout engine:** Given an available output rectangle, ordered windows, gap/border settings, and master ratio, produce non-overlapping rectangles. Account for output scale and reserved space when converting these to surface geometry.
- **Command layer:** Route keybindings and IPC through the same validated commands; publish state changes for external consumers such as a bar.
- **Configuration:** Ship complete defaults. Parse and validate TOML settings and keybindings at startup. Add reload only once applying changes safely is defined.
- **IPC:** Use a per-user Unix socket under `XDG_RUNTIME_DIR`, with access restricted to that user. Start with commands and state queries; add subscriptions when a bar needs them. Document the wire format and error responses.

## Milestones and exit criteria

### 0. Establish the development baseline

- Create the Cargo project, module boundaries, formatting/lint checks, and a brief local run guide.
- Select a Smithay version and its required system dependencies; pin versions for reproducible builds.
- Identify a nested host compositor and a small Wayland client for smoke tests.

**Done when:** a clean build succeeds and a documented command starts the compositor development harness.

### 1. Display one client in a nested, GPU-backed session

- Bring up a nested output and accept an xdg-shell toplevel.
- Render its contents using an explicitly selected GPU-backed renderer; handle commits, resize/configure, damage, and frame callbacks.
- Handle keyboard and pointer input, focus, and clean client shutdown.
- Confirm from renderer/backend diagnostics that the session actually uses the intended GPU path; do not infer this from successful nested rendering alone.

**Done when:** a test client can open, receive input, resize, redraw, and close without hanging; the active rendering path is recorded in the run guide.

### 2. Implement the minimal window manager

- Introduce internal window IDs, ordered windows, focus history, and independent numbered workspaces on each monitor.
- Implement master/stack tiling, adjustable master ratio, gaps, and borders.
- Add focus next/previous, swap, move to workspace, switch workspace, toggle floating, and fullscreen commands.
- Integrate pointer focus and floating-window placement without breaking keyboard focus. Alt-drag converts a tiled window to floating at its tiled geometry; crossing a monitor transfers it to that monitor's active workspace.
- Test layout geometry and command/state transitions without launching a compositor; smoke-test the integration with multiple real clients.

**Done when:** multiple clients can be opened, focused, rearranged, moved between workspaces, floated, and fullscreened in a nested session.

### 3. Add configuration and control

- Provide default keybindings and a documented TOML schema with clear errors for invalid values or duplicate bindings.
- Expose the command layer over a local Unix socket and build a small `develwmctl` client.
- Support queries for outputs, workspaces, focused window, and window list; add event subscriptions for bar integration when needed.
- Define startup and shutdown behavior for the socket, including stale socket cleanup.

**Done when:** the compositor works without a config file, and both a keybinding and `develwmctl` can perform the same workspace/window command.

### 4. Run directly on hardware

- Add native session/seat access, DRM/KMS outputs, GPU-backed rendering, and libinput integration using Smithay's supported backends.
- Handle output hotplug, output scale/mode changes, and multi-monitor workspace/focus behavior.
- On output removal, transfer its windows to a remaining monitor's active workspace, retaining floating state and clamping floating geometry; retain state if no monitor is connected.
- Establish predictable behavior when a device, renderer, or output disappears; avoid leaving a stuck session after exit.
- Validate on target Linux hardware and at least one real multi-monitor setup if available.

**Done when:** the compositor starts from a login session, runs without another Wayland compositor, accepts input, and displays clients on connected monitors.

### 5. Reach daily-driver quality

- Support the essential Wayland desktop functionality required by target applications: clipboard/data-device, primary selection where needed, drag-and-drop, per-output layer-shell bars/launchers, cursor handling, and idle inhibition.
- Provide a secure session-lock path before treating native sessions as daily-driver ready; verify interaction with suspend/resume and VT switching.
- Add XWayland if the target application set needs X11 compatibility; document its status either way.
- Check common workflows: terminal and browser use, copy/paste, application launch, fullscreen, monitor hotplug, screen sharing if required, and recovery after client crashes.
- Document installation, launch, configuration, IPC, troubleshooting, and known limitations.

**Done when:** the defined application/workflow checklist passes on the target hardware for sustained use, with no known issue that prevents normal login, locking, input, or display recovery.

## Scope boundaries

- No animations, blur, shadows, rounded-corner effects, or custom shader effects in v1.
- No i3-style split tree or dwm-style multi-tag views in v1; monocle can wait until after master/stack and floating work.
- No built-in status bar: expose state over IPC and support external bars through layer-shell.
- Direct scanout is a later optimization; first make GPU composition and frame scheduling correct.
- Nested operation under X11 is a development harness; native Wayland operation is required for the standalone release.

## Key risks and validation

| Risk | Early validation |
| --- | --- |
| Nested rendering silently uses an unintended backend | Log renderer/backend selection and verify it during milestone 1 |
| Wayland configure/commit or focus handling diverges from abstract WM state | Use real-client smoke tests alongside pure layout tests |
| Native graphics/input support takes longer than tiling | Finish the one-window vertical slice before expanding WM features |
| Multi-monitor workspace semantics become ambiguous | Define which workspaces are visible per output before milestone 4 implementation |
| Daily-driver protocols are underestimated | Track application workflows and required protocols in milestone 5 |

## Open decisions before implementation

1. ~~Which Linux distribution, GPUs, and host compositor are the initial development/test targets?~~ Manjaro Linux, X11/i3, NVIDIA RTX 5070 for the first nested test.
2. ~~Should workspaces belong to outputs independently, or form one global set that can move between outputs?~~ Each monitor owns its own workspaces and active view; windows can transfer between monitors by dragging.
3. Which terminal, browser, bar, launcher, and screen-sharing tools define the daily-driver acceptance checklist?
4. Is XWayland required for the first daily-driver release, based on those applications?

Resolve the remaining application and compatibility questions as their milestones approach.
