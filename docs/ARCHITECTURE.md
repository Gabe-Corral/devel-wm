# devel-wm architecture

This document records the agreed design for a standalone Rust Wayland compositor. [ROADMAP.md](ROADMAP.md) tracks implementation. The current Smithay/winit code is a bring-up prototype, **not** an implementation of the WM architecture below.

## Product and interaction decisions

- Prioritize **performance and everyday usability** over novel features. A small, keyboard-first compositor: dwm-like master/stack behavior with i3-like runtime configuration and commandability. No animations or visual effects; use GPU-backed rendering efficiently.
- **Exclusive workspaces, not tags.** Each window belongs to exactly one workspace on exactly one monitor. Workspace `1` on monitor A and workspace `1` on monitor B are distinct.
- Each monitor owns its active workspace, workspace contents, tiling order, and focus state. Keyboard input goes to the focused window on the active monitor. Changing one monitor's view does not change another's.
- The initial tiled layout is **master/stack**; floating is a per-window state on the same workspace. Both are required for the MVP. Monocle, a global floating layout, and i3-style split trees are deferred.
- **Alt** is the default modifier for keyboard shortcuts, which are configurable at startup. Alt + left-button dragging remains a fixed pointer binding for now: a tiled window becomes floating at its current on-screen size and position before moving; a floating window keeps its size. Releasing the button leaves it floating. A separate toggle can return it to tiling.
- Dragging across a monitor boundary transfers the window to the destination monitor's **currently active workspace**. A window is never shown simultaneously on two monitors.
- Toggling a tiled window to floating for the first time starts it at its tiled size and location. Remember adjusted floating geometry for later toggles; toggling back places it into the tiling order.
- On monitor disconnect, move its windows to the active workspace on a remaining monitor (prefer the currently active monitor if it survives), keep each window's tiled/floating state, and clamp floating positions to the destination's usable area. The behavior when *no* monitor remains is to retain window state until an output returns; do not discard clients. Monitor reconnect does not automatically move transferred windows back.
- Each monitor has its **own external status bar**. The compositor supports a layer-shell bar on each output, reserves its exclusive zone in the usable area, and exposes per-monitor state through IPC. Bar UI is outside the compositor.

## Ownership and data model

Keep policy types free of Smithay and Wayland objects. The following is a model, not a frozen Rust API:

```text
WmState
├── monitors: MonitorId → MonitorState
│   ├── active_workspace: WorkspaceId
│   ├── workspaces: WorkspaceId → WorkspaceState
│   │   ├── tiled_order: [WindowId]
│   │   ├── floating: [WindowId]
│   │   ├── focused_window: optional WindowId
│   │   └── master_ratio, master_count, gaps
│   └── usable_area: logical rectangle
├── windows: WindowId → WindowState
│   ├── owner: (MonitorId, WorkspaceId)
│   ├── mode: tiled | floating | fullscreen
│   └── floating_geometry: optional logical rectangle
└── active_monitor: optional MonitorId
```

`WindowId` and `MonitorId` are opaque identifiers. A `WorkspaceId` is meaningful **within its monitor**, never by itself. Each monitor maintains at least one workspace. Each live window has one owner and occurs once in either the tiled or floating order of that workspace. Fullscreen is a window state whose prior state must be recoverable. Focus must refer to a live, visible window or be empty. Transferring and closing windows update ownership, orders, and focus together.

Use output-local **logical** coordinates for floating positions and layout decisions. Convert to physical pixels at the rendering boundary using the output's scale and transform. When moving a window to another output, translate its geometry to the destination's local coordinates, then clamp it to the usable area; don't store physical pixel coordinates as WM policy.

## Responsibilities and event flow

```text
Smithay client/input/output events        config / local IPC requests
               │                                     │
               └─────────── typed commands ─────────┘
                                  │
                                  ▼
                   WM core + pure layout calculations
                                  │
                     state / desired geometry / focus
                                  │
                                  ▼
                Smithay adapter: surfaces, seats, configure
                                  │
                     rendered scene + damage/frame events
                                  │
                     nested backend | native backend
```

- **WM core:** Own monitor/workspace/window policy, selection, transfers, focus, and typed commands. Test commands and invariants without starting Wayland. Do not put `WlSurface`, Smithay `Window`, or `Space<Window>` in core state.
- **Layout:** Given a usable logical area, ordered tiled IDs, master settings, and gaps, compute rectangles. Floating windows retain their own geometry and do not consume master/stack slots. Keep layout calculations deterministic and independent of GPU/backend details.
- **Future automation boundary:** Preserve opaque IDs, typed commands/results, and a pure layout input/output model so a later scripting client can inspect and request changes without owning compositor objects. The current read-only IPC snapshots are not yet an ordered event API. Do not add Python hooks to master/stack or a speculative plugin framework just to preserve this option.
- **Wayland/Smithay adapter:** Map `WindowId` to xdg-shell toplevels, handle lifecycle and commits, translate input into commands, apply desired sizes/positions and focus through Smithay, and handle configure/ack/commit timing. Smithay's `Space<Window>` is an integration/rendering representation, **not** the authority for workspace membership or tiling order.
- **Input:** Match configurable shortcuts before forwarding unmatched keyboard events. The nested prototype handles Alt-drag in compositor input state, swallowing client pointer events during the move. A proper pointer grab and cross-output transfer remain necessary for native multi-monitor behavior. Pointer and keyboard focus must agree with the WM core after commands and disconnects.
- **Outputs/backends:** The current winit backend opens a nested window inside X11/i3 for development. A later native backend owns DRM/KMS outputs, session/seat access, and libinput. Both feed events into the same WM core; avoid a speculative large backend trait until there are two real implementations to share.
- **Rendering and scheduling:** Compose clients with the GPU, track damage, and request frames in response to commits, input, and output changes. Send callbacks after an appropriate repaint. The nested prototype now stops requesting redraws while idle, but still redraws the full output when a repaint is needed; partial-damage optimization remains to be done.
- **Layer-shell and bars:** Associate bar surfaces and exclusive zones with individual outputs. Calculate the usable rectangle *after* exclusive zones; provide per-monitor workspace, focus, and layout events to external bars over IPC.
- **Config and IPC:** Provide usable defaults. Parse TOML into validated configuration; make keybindings and Unix-socket IPC use the same typed command path, with per-user socket access under `XDG_RUNTIME_DIR`. IPC is distinct from the Wayland display socket.

Keep modules small and responsibility-driven as implementation grows; the existing placeholder `mod.rs` files are not commitments to a large hierarchy. In particular, nested rendering currently lives in `src/compositor/nested.rs` and must not dictate the pure WM model.

## Future scripting direction (after MVP)

Evaluate [RustPython](https://github.com/RustPython/RustPython), including embedding, alongside a restartable external Python worker for **optional** user automation: react to window/workspace/output events and submit typed WM commands. Keep TOML defaults and the Rust WM core functional without Python. Both scripting and IPC must use the same command/event model; scripts must not own Smithay surfaces, WM state, the input event loop, or rendering. Layout computation stays in Rust for the MVP; Python-defined layouts would need separate evaluation for latency and a deterministic fallback.

The motivating product workflow is an inspectable developer desktop: launch a project, organize its terminal/editor windows, explain each placement, and correct it interactively from a REPL. Treat process details as fallible, potentially sensitive observations rather than window identity: PIDs can be reused, clients can act for other processes, and cwd can be unavailable or misleading. Restoring a project means relaunching applications and reconstructing intent, never reviving old Wayland surfaces. Start with a versioned snapshot/command/event boundary with ordered events, explicit errors, bounded history, and a resync path; keep explanations outside input/rendering hot paths.

An exception or slow/hung script must not block input or frame delivery or bring down the compositor. Before choosing an in-process worker versus a separately restartable scripting process, prototype failure recovery, message ordering, and execution limits; do not assume merely catching exceptions protects against an infinite loop. RustPython describes embedding as supported while noting that it is still in development. Add the runtime only when its startup cost, memory use, and event latency have been measured against the performance goals.

Hot reload must validate and install new handlers before retiring working ones; a failed load must not disable defaults. Already executed commands cannot generally be rolled back. An in-process thread alone cannot guarantee recovery from hung user code, so compare it with a restartable out-of-process worker before promising fault isolation.

## Quality bar

- **Performance:** An unchanged, idle output does not continuously repaint. On active outputs, measure frame timing and dropped/missed frames on representative hardware and client workloads before claiming an optimization helps. Use damage-aware rendering and correct frame callbacks; investigate direct scanout only after composition and scheduling work reliably.
- **Usability:** Starting a native session, launching apps, switching monitor-local workspaces, focusing/tiling/floating, moving between monitors, resizing, copy/paste, locking/unlocking, and recovering from output changes must work predictably. Defaults must be usable without writing a config file, while Alt and other bindings remain configurable.
- **Prioritization:** Optional RustPython scripting, explainable placement, event history, advanced inspection, developer sessions, and other differentiators are follow-ups, not MVP requirements. Keep the basic IPC needed for commands and per-monitor bars in scope.

## Known prototype gaps

Today the program opens a GPU-rendered nested output with a pure WM core, monitor-local workspaces, master/stack tiling, floating/toggle/Alt-drag support, validated TOML keyboard bindings, layer-shell bars, and a read-only IPC state feed. Custom bindings still need manual input verification. Native outputs, configurable pointer bindings, cross-output floating moves, IPC commands, decorations, interactive xdg move/resize, popup grabs, cursor image rendering, and partial-damage rendering remain incomplete. A client connection and GPU renderer log do not by themselves prove the complete interactive experience; use the smoke tests in the roadmap.
