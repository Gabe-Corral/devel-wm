# Explicit xdg popup grab smoke test

The compositor tracks xdg popups and uses Smithay's popup keyboard/pointer
grabs when a focused client requests `xdg_popup.grab` with an input serial.
This path is separate from initial popup positioning and repositioning.

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
For protocol verification, use a client known to issue `xdg_popup.grab` and
inspect Wayland protocol traces. This smoke test has not yet been manually run
against the new grab implementation.
