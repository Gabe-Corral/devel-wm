# Explicit xdg popup grab smoke test

The compositor tracks xdg popups and uses Smithay's popup keyboard/pointer
grabs when a focused client requests `xdg_popup.grab` with an input serial.
This path is separate from initial popup positioning and repositioning.

**Nested-backend verification:** Firefox context menus were visually checked
near output edges. The popup test client logs showed initial and nested grab
requests followed by popup configures, matching reposition tokens 1–4, and
`popup_done` for the child before the parent. Native/multi-output behavior,
Escape cleanup, and first-click focus on another client remain separate checks.

In the nested compositor, launch native Wayland Firefox with a separate profile
(see the project README for the current `WAYLAND_DISPLAY` socket). With a second
Wayland client open:

1. Open a Firefox context menu. Use the pointer and keyboard inside the menu;
   opening a submenu should not switch workspace or send input to the other
   client.
2. Press Escape. The menu should close, and keyboard input should return to
   Firefox. Repeat with a click on the other client: the menu should dismiss,
   and the clicked client should take focus on that click.
3. Open another menu after dismissal. Check that it receives pointer and
   keyboard input; no stale grab should remain. Close Firefox with a menu open
   and verify the other client still accepts input.

Firefox may implement some menus without requesting `xdg_popup.grab`; observing
normal menu behavior alone does not prove this protocol path was exercised.
For protocol verification, use the [popup test client](../popup-test/README.md),
which deliberately requests a grab before its first commit and can open a
nested grab. Firefox alone does not verify grabs.

## Positioning and repositioning

With Firefox on the nested output, open context menus near each output edge and
corner, including after resizing or moving its window. Menus requesting flip,
slide, or resize constraint adjustments should remain within the output when
possible. Test a submenu near an edge as well: its positioner is relative to
the parent popup, not the Firefox toplevel. Move a window with a reactive popup
open and verify its placement updates. The popup test client also issues
`xdg_popup.reposition` and checks for a matching `repositioned` token and
configure. That protocol path cannot be inferred from Firefox menus alone.
