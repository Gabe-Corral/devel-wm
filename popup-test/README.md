# Popup protocol test client

Standalone native Wayland client for exercising devel-wm's explicit
`xdg_popup.grab`, nested popup, and `xdg_popup.reposition` handling. It uses
Smithay Client Toolkit 0.19.2 (matching the separate `bar-test` client); it is
not part of the compositor or the root Cargo package.

Start devel-wm in the X11 session with `cargo run --locked`, copy the
`WAYLAND_DISPLAY` name it prints, then in another terminal run (replace
`wayland-N`):

```sh
env -u DISPLAY WAYLAND_DISPLAY=wayland-N \
  cargo run --manifest-path popup-test/Cargo.toml --locked
```

The window titled **Popup protocol test** is the blue parent, the green popup
is grabbed, and the red popup is a nested grab. Click the blue *content below
the titlebar*, not the compositor-drawn titlebar or the separate prototype
bar. Run the client from the repository root or use the full path to its
manifest. The terminal prints `window configured`, `pointer entered`, and
`pointer press` diagnostics to distinguish focus/hit-testing failures from
popup-protocol failures. The test surface deliberately stays 360×240 even
when a tiling compositor requests a larger window; only click inside its
visible blue area. SHM allocations are capped (32 small buffers per run), and
subsequent unchanged configures reuse the existing image.

1. Left-click the blue window: the client requests `xdg_popup.grab` using the
   actual pointer-press serial **before** the popup's first commit. Expect a
   `requested grab` message followed by an initial popup configure and a
   visible green popup.
2. Left-click the green popup to request a reposition. The green popup should
   move, and the terminal should print `PASS: reposition token N acknowledged
   with configure`. Repeat to toggle between two positions. This confirms the
   matching `repositioned` token and configure; it does not by itself test
   every edge constraint adjustment.
3. Right-click the green popup to open a red child popup with a nested grab.
   Check that it receives input and that clicking another client dismisses
   the popup(s), prints `popup_done`, and focuses the clicked client. If the
   parent also receives `popup_done`, the red popup must be destroyed first.
4. Reopen the popup, press Escape to destroy it, and verify another client can
   still receive input. Use Ctrl-C in the launching terminal to exit.

To check what was sent on the wire, add `WAYLAND_DEBUG=client` and redirect
stderr to a log. The useful messages contain `xdg_popup#ID.grab(...)`,
`xdg_popup#ID.reposition(...)`, and `xdg_popup#ID.repositioned(...)`.
This client builds and passes Clippy without an active Wayland compositor, but
the interactive scenarios require a running compositor and have not yet been
manually verified.
